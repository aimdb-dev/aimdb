//! `mqtts://` on the host: the embedded backend against a local broker whose
//! self-signed certificate is pinned as the root CA (`_test-tls-broker`).
//!
//! The first host coverage the TLS path has had. It runs the same
//! `embedded-tls` session an MCU runs, over `TokioNet::tcp()`, with the clock
//! from the runtime's wall clock and no SNTP task.
#![cfg(feature = "_test-tls-broker")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::SeedableRng as _;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

mod common;
use common::{serve_stream, AfterSuback, Seen};

// Each test binary defines these exactly once.
#[defmt::global_logger]
struct HostTestLogger;
unsafe impl defmt::Logger for HostTestLogger {
    fn acquire() {}
    unsafe fn flush() {}
    unsafe fn release() {}
    unsafe fn write(_bytes: &[u8]) {}
}
#[defmt::panic_handler]
fn defmt_panic() -> ! {
    core::panic!("defmt panic in host test")
}
defmt::timestamp!("{=u64:us}", 0);

/// The name the certificate is issued for, and the name the client verifies.
/// A hostname rather than an IP literal: `rustpki` matches an IP only through
/// the CN fallback, which is a narrower path than this test should depend on.
const BROKER_HOST: &str = "localhost";

/// A self-signed certificate for `localhost`, returned as (server chain,
/// server key, root CA in DER) — the same bytes on both sides, which is what
/// "pinned root" means.
fn self_signed() -> (
    CertificateDer<'static>,
    PrivateKeyDer<'static>,
    &'static [u8],
) {
    let cert = rcgen::generate_simple_self_signed(vec![BROKER_HOST.to_string()])
        .expect("generate self-signed certificate");
    let der = cert.cert.der().to_vec();
    let key = PrivateKeyDer::try_from(cert.key_pair.serialize_der()).expect("server key");
    // `&'static` because `TlsOptions` holds the trust root for the session's
    // whole life; one leak per test process.
    let ca: &'static [u8] = Box::leak(der.clone().into_boxed_slice());
    (CertificateDer::from(der), key, ca)
}

/// Accept TLS connections and serve the same fake MQTT broker over them.
async fn tls_broker(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    seen: Arc<Mutex<Seen>>,
    push: Option<(&'static str, &'static [u8])>,
) {
    loop {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let acceptor = acceptor.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            let Ok(mut stream) = acceptor.accept(socket).await else {
                return;
            };
            let after = AfterSuback {
                hang_up: false,
                push,
            };
            serve_stream(&mut stream, &seen, after).await;
        });
    }
}

/// A `mqtts://` session completes and round-trips a record, with the
/// certificate verified against the pinned root and the clock from
/// `SystemTime` — no SNTP anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_embedded_backend_completes_an_mqtts_handshake_against_a_pinned_root() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::{MqttConnector, TlsOptions};
    use aimdb_tokio_adapter::net::TokioNet;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let (chain, key, ca_der) = self_signed();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![chain], key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));

    // `TlsOptions` holds `&'static mut` buffers and RNG: on a board these are
    // `StaticCell`s, here one leak apiece.
    let rng: &'static mut (dyn embedded_tls::CryptoRngCore + Send) =
        Box::leak(Box::new(rand::rngs::StdRng::from_entropy()));
    let read_buf: &'static mut [u8] = Box::leak(vec![0u8; 16_640].into_boxed_slice());
    let write_buf: &'static mut [u8] = Box::leak(vec![0u8; 4_096].into_boxed_slice());

    let connector = MqttConnector::new(format!("mqtts://{BROKER_HOST}:{port}"))
        .tls(
            TokioNet::tcp(),
            TlsOptions::new(rng, ca_der, read_buf, write_buf),
        )
        .with_client_id("tls-host-smoke");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);
    builder.configure::<u64>("temperature", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("mqtt://sensors/temperature")
            .with_deserializer(|_ctx, data: &[u8]| {
                core::str::from_utf8(data)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .ok_or_else(|| String::from("bad payload"))
            })
            .finish();
    });

    let (db, runner) = builder.build().await.expect("build db");
    let mut inbound = db
        .consumer::<u64>("temperature")
        .expect("temperature consumer")
        .subscribe();

    let broker = tls_broker(
        listener,
        acceptor,
        seen.clone(),
        Some(("sensors/temperature", b"23")),
    );

    let received = tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = broker => panic!("the broker returned"),
        value = inbound.recv() => value.expect("inbound record"),
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: {} connects, {:?} subscribed — the handshake never completed",
                seen.connects,
                seen.subscribed_topics()
            );
        }
    };

    assert_eq!(
        received, 23,
        "the message must arrive through the TLS session"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.connects, 1, "exactly one MQTT session over TLS");
    assert!(
        seen.subscribed_topics().contains(&"sensors/temperature"),
        "the session must subscribe over TLS; saw {:?}",
        seen.subscribed_topics()
    );
}
