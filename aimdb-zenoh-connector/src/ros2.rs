//! `Ros2Connector`: `ros2://` links following rmw_zenoh's conventions.

use std::any::{type_name, TypeId};
use std::boxed::Box;
use std::format;
use std::string::String;
use std::time::{SystemTime, UNIX_EPOCH};
use std::vec::Vec;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::{
    log_error, log_warn, AimDb, ExactGrammar, InboundDispatch, OutboundRoutes, RouteInfo,
};
use aimdb_data_contracts::{ros2, RosMessage, WireFormat};
use zenoh::key_expr::KeyExpr;
use zenoh::qos::CongestionControl;
use zenoh::sample::Locality;

use crate::connector::{BoxFuture, BuildFuture};
use crate::native::{config_error, open, session_config};
use crate::profile::{self, Attachment, Durability, EntityKind, History, Node, Qos, Reliability};

/// The URL scheme of ROS 2 topic links.
pub(crate) const SCHEME: &str = "ros2";

/// The environment variable ROS reads the domain from.
const DOMAIN_ENV: &str = "ROS_DOMAIN_ID";

/// Link config key for [`Ros2LinkExt::with_depth`](crate::Ros2LinkExt::with_depth).
pub(crate) const DEPTH_KEY: &str = "ros2.depth";
/// Link config key for [`Ros2LinkExt::with_reliability`](crate::Ros2LinkExt::with_reliability).
pub(crate) const RELIABILITY_KEY: &str = "ros2.reliability";

/// The highest domain id ROS 2 documents.
const DOMAIN_MAX: u32 = 232;

/// The ROS 2 node AimDB appears as.
#[derive(Debug, Clone)]
pub struct Ros2Node {
    name: String,
    namespace: String,
}

impl Ros2Node {
    /// A node in the root namespace.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: String::from("/"),
        }
    }

    /// The node's namespace, e.g. `/cell4`. It sets the node's identity, not
    /// its topics: a `ros2://` link always names a fully qualified topic.
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }
}

/// A type registered with [`Ros2Connector::register`].
#[derive(Debug, Clone)]
struct RosType {
    type_id: TypeId,
    rust_name: &'static str,
    dds_name: &'static str,
    hash: &'static str,
    wire_format: WireFormat,
}

/// A connector for `ros2://` links: AimDB records as ROS 2 topics, as
/// rmw_zenoh puts them on the wire.
///
/// ```rust,ignore
/// let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447", Ros2Node::new("cell4_gateway"))
///     .register::<Temperature>();
/// builder.with_connector(ros2);
/// ```
pub struct Ros2Connector {
    endpoint: String,
    config: Option<zenoh::Config>,
    node: Ros2Node,
    domain: Option<u32>,
    types: Vec<RosType>,
}

impl Ros2Connector {
    /// Connect to the Zenoh router at `endpoint` (`tcp/host:port`) as `node`.
    pub fn new(endpoint: impl Into<String>, node: Ros2Node) -> Self {
        Self {
            endpoint: endpoint.into(),
            config: None,
            node,
            domain: None,
            types: Vec::new(),
        }
    }

    /// The ROS domain. Without it the connector reads `ROS_DOMAIN_ID`, as
    /// `rcl` does, and falls back to 0.
    pub fn domain_id(mut self, domain: u32) -> Self {
        self.domain = Some(domain);
        self
    }

    /// Allow `ros2://` links on records of type `T`.
    pub fn register<T: RosMessage + 'static>(mut self) -> Self {
        self.types.push(RosType {
            type_id: TypeId::of::<T>(),
            rust_name: type_name::<T>(),
            dds_name: T::ROS_TYPE_NAME,
            hash: T::ROS_TYPE_HASH,
            wire_format: T::WIRE_FORMAT,
        });
        self
    }

    /// Start from `config` instead of Zenoh's defaults in client mode; see
    /// [`ZenohConnector::with_zenoh_config`](crate::ZenohConnector::with_zenoh_config).
    pub fn with_zenoh_config(mut self, config: zenoh::Config) -> Self {
        self.config = Some(config);
        self
    }
}

impl ConnectorBuilder for Ros2Connector {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        Box::pin(async move {
            let domain = resolve_domain(self.domain, std::env::var(DOMAIN_ENV).ok().as_deref())
                .map_err(config_error)?;
            let node = &self.node;
            profile::validate_node_name(&node.name)
                .map_err(|e| config_error(format!("node name '{}' {e}", node.name)))?;
            profile::validate_namespace(&node.namespace)
                .map_err(|e| config_error(format!("namespace '{}' {e}", node.namespace)))?;
            for ty in &self.types {
                check_type(ty).map_err(config_error)?;
            }

            let inbound = InboundDispatch::new(db, SCHEME, &ExactGrammar)?;
            if inbound.route_count() > 0 {
                return Err(config_error(String::from(
                    "inbound ros2:// links are not supported yet",
                )));
            }

            let outbound = OutboundRoutes::new(db, SCHEME)?;
            let routes = outbound
                .routes()
                .iter()
                .enumerate()
                .map(|(i, route)| plan(route, &self.types, domain, i as u32 + 1))
                .collect::<Result<Vec<_>, _>>()
                .map_err(config_error)?;

            let config = session_config(&self.endpoint, self.config.clone())?;
            let task: BoxFuture =
                Box::pin(run(config, domain, self.node.clone(), outbound, routes));
            Ok(Vec::from([task]))
        })
    }

    fn scheme(&self) -> &str {
        SCHEME
    }

    fn owns_scheme(&self) -> bool {
        true
    }
}

/// The domain `rcl` would use: the explicit one, else `ROS_DOMAIN_ID` unless
/// it is empty, else 0. ROS 2 documents domains 0 to 232; a higher one would
/// put the node where no other node is.
fn resolve_domain(explicit: Option<u32>, env: Option<&str>) -> Result<u32, String> {
    let env = env.map(str::trim).filter(|v| !v.is_empty());
    let (domain, source) = match (explicit, env) {
        (Some(domain), _) => (domain, "domain_id(..)"),
        (None, Some(value)) => {
            let domain = value
                .parse()
                .map_err(|_| format!("{DOMAIN_ENV}='{value}' is not a domain id"))?;
            (domain, DOMAIN_ENV)
        }
        (None, None) => return Ok(0),
    };
    if domain > DOMAIN_MAX {
        return Err(format!(
            "domain {domain} from {source} is above {DOMAIN_MAX}, the highest ROS 2 domain"
        ));
    }
    Ok(domain)
}

/// A registered type's own claims: it is CDR, and its names are well formed.
/// The derive guarantees all three; a hand-written impl may not.
fn check_type(ty: &RosType) -> Result<(), String> {
    if ty.wire_format != WireFormat::Cdr {
        return Err(format!(
            "{} is registered for ROS 2, but its Linkable encoding is not CDR",
            ty.rust_name
        ));
    }
    ros2::validate_dds_type_name(ty.dds_name)
        .map_err(|e| format!("{}: ROS type name '{}': {e}", ty.rust_name, ty.dds_name))?;
    ros2::validate_type_hash(ty.hash)
        .map_err(|e| format!("{}: type hash '{}': {e}", ty.rust_name, ty.hash))?;
    Ok(())
}

/// Everything one outbound route needs on the wire, fixed at build.
struct Route {
    topic: String,
    ty: RosType,
    qos: Qos,
    entity_id: u32,
    key: KeyExpr<'static>,
}

fn plan(
    route: &RouteInfo,
    types: &[RosType],
    domain: u32,
    entity_id: u32,
) -> Result<Route, String> {
    let resource = &*route.default_topic;
    let topic = format!("/{resource}");
    profile::validate_topic(&topic).map_err(|e| format!("ROS topic '{topic}' {e}"))?;
    if route.topic_capacity > 0 {
        return Err(format!(
            "ros2://{resource} has a topic writer; a ROS topic is fixed by its link"
        ));
    }
    let ty = types
        .iter()
        .find(|t| t.type_id == route.type_id)
        .cloned()
        .ok_or_else(|| {
            format!("ros2://{resource}: the record's type is not registered with Ros2Connector")
        })?;
    let options = &route.config.protocol_options;
    match WireFormat::recorded_in(options) {
        WireFormat::Cdr => {}
        WireFormat::Unspecified => log_warn!(
            "ros2://{}: a custom serializer is installed; nothing checks that it writes CDR",
            resource
        ),
        other => {
            return Err(format!(
                "ros2://{resource} encodes as {other:?}; a ROS topic needs CDR"
            ))
        }
    }
    let qos = qos(options).map_err(|e| format!("ros2://{resource}: {e}"))?;
    let key = KeyExpr::try_from(profile::data_key(domain, &topic, ty.dds_name, ty.hash))
        .map_err(|e| format!("ros2://{resource}: {e}"))?;
    Ok(Route {
        topic,
        ty,
        qos,
        entity_id,
        key,
    })
}

/// The link's QoS overrides on top of the rmw default profile.
fn qos(options: &[(String, String)]) -> Result<Qos, String> {
    let mut qos = Qos::default();
    for (key, value) in options {
        match key.as_str() {
            DEPTH_KEY => {
                qos.depth =
                    value.parse().ok().filter(|d| *d > 0).ok_or_else(|| {
                        format!("depth must be a positive integer, got '{value}'")
                    })?;
            }
            RELIABILITY_KEY => {
                qos.reliability = Reliability::parse(value).ok_or_else(|| {
                    format!("reliability must be reliable or best_effort, got '{value}'")
                })?;
            }
            _ => {}
        }
    }
    debug_assert!(qos.durability == Durability::Volatile && qos.history == History::KeepLast);
    Ok(qos)
}

/// Open the session, declare the node and every publisher's liveliness
/// token, then publish each value with its attachment.
async fn run(
    config: zenoh::Config,
    domain: u32,
    node: Ros2Node,
    mut outbound: OutboundRoutes,
    routes: Vec<Route>,
) {
    let session = open(config).await;
    let zid = session.zid().to_string();
    let identity = Node {
        domain,
        zid: &zid,
        id: 0,
        enclave: "/",
        namespace: &node.namespace,
        name: &node.name,
    };

    let mut tokens = Vec::with_capacity(routes.len() + 1);
    tokens.push(profile::node_token(&identity));
    let mut publishers = Vec::with_capacity(routes.len());
    let mut gids = Vec::with_capacity(routes.len());
    for route in &routes {
        let token = profile::entity_token(
            &identity,
            route.entity_id,
            EntityKind::Publisher,
            &route.topic,
            route.ty.dds_name,
            route.ty.hash,
            &route.qos,
        );
        gids.push(profile::gid(&token));
        tokens.push(token);
    }
    // Declared before the first publish, as rmw_zenoh does.
    let mut live = Vec::with_capacity(tokens.len());
    for token in tokens {
        match session.liveliness().declare_token(token).await {
            Ok(token) => live.push(token),
            Err(_e) => log_error!("ROS 2: cannot declare a liveliness token: {}", _e),
        }
    }
    for route in &routes {
        let publisher = session
            .declare_publisher(route.key.clone())
            .congestion_control(CongestionControl::Drop)
            .allowed_destination(Locality::Remote)
            .await;
        match publisher {
            Ok(publisher) => publishers.push(Some(publisher)),
            Err(_e) => {
                log_error!(
                    "ROS 2: cannot declare a publisher on '{}': {}",
                    route.topic,
                    _e
                );
                publishers.push(None);
            }
        }
    }

    let mut sequence = std::vec![0i64; routes.len()];
    while let Some(msg) = outbound.next().await {
        let id = msg.route.id;
        let Some(publisher) = &publishers[id] else {
            outbound.reject(id);
            continue;
        };
        sequence[id] += 1;
        let attachment = Attachment {
            sequence: sequence[id],
            timestamp_ns: now_ns(),
            gid: gids[id],
        };
        let result = publisher
            .put(msg.payload.into_vec())
            .attachment(attachment.encode())
            .await;
        if let Err(_e) = result {
            log_error!("ROS 2: put on '{}' failed: {}", routes[id].topic, _e);
            outbound.reject(id);
        }
    }
    // The tokens keep the node in the ROS graph for the life of the database.
    let _live = live;
    core::future::pending::<()>().await;
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_resolves_as_rcl_resolves_it() {
        assert_eq!(resolve_domain(Some(7), Some("3")), Ok(7), "explicit wins");
        assert_eq!(
            resolve_domain(None, Some("3")),
            Ok(3),
            "then the environment"
        );
        assert_eq!(resolve_domain(None, Some(" 12 ")), Ok(12));
        assert_eq!(resolve_domain(None, None), Ok(0), "then 0");
        assert_eq!(resolve_domain(None, Some("")), Ok(0), "empty is unset");
        assert_eq!(resolve_domain(None, Some("  ")), Ok(0), "blank is unset");
        assert_eq!(resolve_domain(Some(232), None), Ok(232));
        assert_eq!(resolve_domain(None, Some("232")), Ok(232));
        let high = resolve_domain(Some(233), None).unwrap_err();
        assert!(high.contains("233 from domain_id(..)"), "{high}");
        let high = resolve_domain(None, Some("233")).unwrap_err();
        assert!(high.contains("233 from ROS_DOMAIN_ID"), "{high}");
        assert!(resolve_domain(None, Some("4294967295")).is_err());
        let bad = resolve_domain(None, Some("seven")).unwrap_err();
        assert!(bad.contains("ROS_DOMAIN_ID='seven'"), "{bad}");
        assert!(resolve_domain(None, Some("-1")).is_err());
    }

    #[test]
    fn link_options_override_the_default_qos() {
        let opt = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(qos(&[]).unwrap(), Qos::default());
        let q = qos(&[opt(DEPTH_KEY, "1"), opt(RELIABILITY_KEY, "best_effort")]).unwrap();
        assert_eq!((q.depth, q.reliability), (1, Reliability::BestEffort));
        assert!(qos(&[opt(DEPTH_KEY, "0")]).is_err());
        assert!(qos(&[opt(DEPTH_KEY, "ten")]).is_err());
        assert!(qos(&[opt(RELIABILITY_KEY, "maybe")]).is_err());
    }
}
