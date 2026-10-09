//! The profile against values captured from real rmw_zenoh nodes: Jazzy
//! (rmw_zenoh 0.2.11) and Lyrical (0.10.6), both with zenoh-c 1.8.0.
//!
//! Captured with a `zenoh` client on the router: `demo_nodes_cpp` talkers,
//! one in domain 7 under `/cell4`, and `ros2 topic pub` with depth 42, depth
//! 10, best-effort, transient-local and keep-all. Each attachment comes from
//! a sample of the publisher on the line above it.

use alloc::vec::Vec;

use super::*;

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The attachment decodes, carries the GID of `token`, and re-encodes to the
/// same bytes.
fn check_attachment(hex: &str, token: &str) {
    let captured = bytes(hex);
    let attachment = Attachment::decode(&captured).expect("rmw_zenoh layout");
    assert_eq!(attachment.gid, gid(token), "GID of {token}");
    assert!(attachment.sequence >= 1);
    assert_eq!(attachment.encode().as_slice(), captured.as_slice());
}

#[test]
fn tokens_keys_and_gids_match_rmw_zenoh() {
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "45000cc3452ec777e9664270c90b5af",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_be"
        }),
        "@ros2_lv/0/45000cc3452ec777e9664270c90b5af/0/0/NN/%/%/p_be"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "45000cc3452ec777e9664270c90b5af", id: 0, enclave: "/", namespace: "/", name: "p_be" }, 4, EntityKind::Publisher, "/q_best_effort", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::BestEffort, durability: Durability::Volatile, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/45000cc3452ec777e9664270c90b5af/0/4/MP/%/%/p_be/%q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/2::,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_best_effort", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("150000000000000058d1b24cf9a2dc1810a4461b1ae3b3b1de4a54c9c5e9e88fd0", "@ros2_lv/0/45000cc3452ec777e9664270c90b5af/0/4/MP/%/%/p_be/%q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/2::,10:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "4d5b53bcdd3d15e71bef6b55f1997400",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_depth42"
        }),
        "@ros2_lv/0/4d5b53bcdd3d15e71bef6b55f1997400/0/0/NN/%/%/p_depth42"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "4d5b53bcdd3d15e71bef6b55f1997400", id: 0, enclave: "/", namespace: "/", name: "p_depth42" }, 4, EntityKind::Publisher, "/q_depth42", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 42 }),
        "@ros2_lv/0/4d5b53bcdd3d15e71bef6b55f1997400/0/4/MP/%/%/p_depth42/%q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_depth42", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1500000000000000c2bec64cf9a2dc1810c4fac2c77aa599d761ccd1c0dd836e24", "@ros2_lv/0/4d5b53bcdd3d15e71bef6b55f1997400/0/4/MP/%/%/p_depth42/%q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "914ec2c920f5151f26394cf0ba959999",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_tl"
        }),
        "@ros2_lv/0/914ec2c920f5151f26394cf0ba959999/0/0/NN/%/%/p_tl"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "914ec2c920f5151f26394cf0ba959999", id: 0, enclave: "/", namespace: "/", name: "p_tl" }, 4, EntityKind::Publisher, "/q_transient", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::TransientLocal, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/914ec2c920f5151f26394cf0ba959999/0/4/MP/%/%/p_tl/%q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/:1:,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_transient", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("15000000000000008903674cf9a2dc1810b9249d3fec6b1be4c0c8a0e3eb3550bb", "@ros2_lv/0/914ec2c920f5151f26394cf0ba959999/0/4/MP/%/%/p_tl/%q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/:1:,10:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "a52f44b281e33efee354a095ad96d9e9",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "talker"
        }),
        "@ros2_lv/0/a52f44b281e33efee354a095ad96d9e9/0/0/NN/%/%/talker"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "a52f44b281e33efee354a095ad96d9e9", id: 0, enclave: "/", namespace: "/", name: "talker" }, 11, EntityKind::Publisher, "/chatter", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 7 }),
        "@ros2_lv/0/a52f44b281e33efee354a095ad96d9e9/0/11/MP/%/%/talker/%chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,7:,:,:,,"
    );
    assert_eq!(data_key(0, "/chatter", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1a0000000000000005301f32f9a2dc18103e1e8c9234b1602a9dba5860eed86e33", "@ros2_lv/0/a52f44b281e33efee354a095ad96d9e9/0/11/MP/%/%/talker/%chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,7:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "b2936fe8c98a79b9e28da840b609862b",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_ka"
        }),
        "@ros2_lv/0/b2936fe8c98a79b9e28da840b609862b/0/0/NN/%/%/p_ka"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "b2936fe8c98a79b9e28da840b609862b", id: 0, enclave: "/", namespace: "/", name: "p_ka" }, 4, EntityKind::Publisher, "/q_keep_all", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepAll, depth: 10 }),
        "@ros2_lv/0/b2936fe8c98a79b9e28da840b609862b/0/4/MP/%/%/p_ka/%q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::2,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_keep_all", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("15000000000000001cfe3a4cf9a2dc1810e6d945421b873f8046a960ce657b3641", "@ros2_lv/0/b2936fe8c98a79b9e28da840b609862b/0/4/MP/%/%/p_ka/%q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::2,10:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "ecb5f8e5211f6643577812e1e59f4666",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_depth10"
        }),
        "@ros2_lv/0/ecb5f8e5211f6643577812e1e59f4666/0/0/NN/%/%/p_depth10"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "ecb5f8e5211f6643577812e1e59f4666", id: 0, enclave: "/", namespace: "/", name: "p_depth10" }, 4, EntityKind::Publisher, "/q_depth10", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/ecb5f8e5211f6643577812e1e59f4666/0/4/MP/%/%/p_depth10/%q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_depth10", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1500000000000000edb12d4df9a2dc18103b5f5f1e26e9b9c14d8b73120159f4d6", "@ros2_lv/0/ecb5f8e5211f6643577812e1e59f4666/0/4/MP/%/%/p_depth10/%q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,10:,:,:,,");
    // jazzy
    assert_eq!(
        node_token(&Node {
            domain: 7,
            zid: "6977b59f87229328a8d406ad77b301a6",
            id: 0,
            enclave: "/",
            namespace: "/cell4",
            name: "gw"
        }),
        "@ros2_lv/7/6977b59f87229328a8d406ad77b301a6/0/0/NN/%/%cell4/gw"
    );
    // jazzy
    assert_eq!(
        entity_token(&Node { domain: 7, zid: "6977b59f87229328a8d406ad77b301a6", id: 0, enclave: "/", namespace: "/cell4", name: "gw" }, 11, EntityKind::Publisher, "/cell4/chatter", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 7 }),
        "@ros2_lv/7/6977b59f87229328a8d406ad77b301a6/0/11/MP/%/%cell4/gw/%cell4%chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,7:,:,:,,"
    );
    assert_eq!(data_key(7, "/cell4/chatter", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "7/cell4/chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1a00000000000000699e1332f9a2dc1810b7aef695b2cd04ba45f9eed0c4a6c584", "@ros2_lv/7/6977b59f87229328a8d406ad77b301a6/0/11/MP/%/%cell4/gw/%cell4%chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,7:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "1cdcdbd45bbff59f7a7a50283650fb64",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "talker"
        }),
        "@ros2_lv/0/1cdcdbd45bbff59f7a7a50283650fb64/0/0/NN/%/%/talker"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "1cdcdbd45bbff59f7a7a50283650fb64", id: 0, enclave: "/", namespace: "/", name: "talker" }, 10, EntityKind::Publisher, "/chatter", "example_interfaces::msg::dds_::String_", "RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 7 }),
        "@ros2_lv/0/1cdcdbd45bbff59f7a7a50283650fb64/0/10/MP/%/%/talker/%chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b/::,7:,:,:,,"
    );
    assert_eq!(data_key(0, "/chatter", "example_interfaces::msg::dds_::String_", "RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b"), "0/chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b");
    check_attachment("170000000000000007565979dda2dc181043fbe3d3dcfb9659b7a82d4478c8eca0", "@ros2_lv/0/1cdcdbd45bbff59f7a7a50283650fb64/0/10/MP/%/%/talker/%chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b/::,7:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "441ed9dd3a332261e0c9d6e43d8cad1d",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_tl"
        }),
        "@ros2_lv/0/441ed9dd3a332261e0c9d6e43d8cad1d/0/0/NN/%/%/p_tl"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "441ed9dd3a332261e0c9d6e43d8cad1d", id: 0, enclave: "/", namespace: "/", name: "p_tl" }, 4, EntityKind::Publisher, "/q_transient", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::TransientLocal, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/441ed9dd3a332261e0c9d6e43d8cad1d/0/4/MP/%/%/p_tl/%q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/:1:,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_transient", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("150000000000000052e623a3dda2dc18100860bd607ea991ace37183d2b2a112ff", "@ros2_lv/0/441ed9dd3a332261e0c9d6e43d8cad1d/0/4/MP/%/%/p_tl/%q_transient/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/:1:,10:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "5466af59ede35c5be9cb5513adf36541",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_be"
        }),
        "@ros2_lv/0/5466af59ede35c5be9cb5513adf36541/0/0/NN/%/%/p_be"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "5466af59ede35c5be9cb5513adf36541", id: 0, enclave: "/", namespace: "/", name: "p_be" }, 4, EntityKind::Publisher, "/q_best_effort", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::BestEffort, durability: Durability::Volatile, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/5466af59ede35c5be9cb5513adf36541/0/4/MP/%/%/p_be/%q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/2::,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_best_effort", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("15000000000000007bca51a3dda2dc1810e67cc94b80d75ed5aa2880df74ac0bc4", "@ros2_lv/0/5466af59ede35c5be9cb5513adf36541/0/4/MP/%/%/p_be/%q_best_effort/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/2::,10:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "93feaf5f1a1e1d73349ed78c24d233ee",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_depth42"
        }),
        "@ros2_lv/0/93feaf5f1a1e1d73349ed78c24d233ee/0/0/NN/%/%/p_depth42"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "93feaf5f1a1e1d73349ed78c24d233ee", id: 0, enclave: "/", namespace: "/", name: "p_depth42" }, 4, EntityKind::Publisher, "/q_depth42", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 42 }),
        "@ros2_lv/0/93feaf5f1a1e1d73349ed78c24d233ee/0/4/MP/%/%/p_depth42/%q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_depth42", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1500000000000000240db5a3dda2dc181024185e5248c67368fab11b9817dd22b9", "@ros2_lv/0/93feaf5f1a1e1d73349ed78c24d233ee/0/4/MP/%/%/p_depth42/%q_depth42/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "96030c84a104a478c5e739aacc9dc763",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_ka"
        }),
        "@ros2_lv/0/96030c84a104a478c5e739aacc9dc763/0/0/NN/%/%/p_ka"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "96030c84a104a478c5e739aacc9dc763", id: 0, enclave: "/", namespace: "/", name: "p_ka" }, 4, EntityKind::Publisher, "/q_keep_all", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepAll, depth: 10 }),
        "@ros2_lv/0/96030c84a104a478c5e739aacc9dc763/0/4/MP/%/%/p_ka/%q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::2,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_keep_all", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("15000000000000007d5de6a3dda2dc181089ee3c7c4ca1012497819746e530a097", "@ros2_lv/0/96030c84a104a478c5e739aacc9dc763/0/4/MP/%/%/p_ka/%q_keep_all/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::2,10:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 0,
            zid: "ff6c41dd9c6b1963c056554e51113e2b",
            id: 0,
            enclave: "/",
            namespace: "/",
            name: "p_depth10"
        }),
        "@ros2_lv/0/ff6c41dd9c6b1963c056554e51113e2b/0/0/NN/%/%/p_depth10"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 0, zid: "ff6c41dd9c6b1963c056554e51113e2b", id: 0, enclave: "/", namespace: "/", name: "p_depth10" }, 4, EntityKind::Publisher, "/q_depth10", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 10 }),
        "@ros2_lv/0/ff6c41dd9c6b1963c056554e51113e2b/0/4/MP/%/%/p_depth10/%q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,10:,:,:,,"
    );
    assert_eq!(data_key(0, "/q_depth10", "std_msgs::msg::dds_::String_", "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"), "0/q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18");
    check_attachment("1500000000000000da84fda2dda2dc1810be2dbfb962d6c6fb80e105afbb1ab12a", "@ros2_lv/0/ff6c41dd9c6b1963c056554e51113e2b/0/4/MP/%/%/p_depth10/%q_depth10/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,10:,:,:,,");
    // lyrical
    assert_eq!(
        node_token(&Node {
            domain: 7,
            zid: "304b88ed8d0d24d941ca41005d509970",
            id: 0,
            enclave: "/",
            namespace: "/cell4",
            name: "gw"
        }),
        "@ros2_lv/7/304b88ed8d0d24d941ca41005d509970/0/0/NN/%/%cell4/gw"
    );
    // lyrical
    assert_eq!(
        entity_token(&Node { domain: 7, zid: "304b88ed8d0d24d941ca41005d509970", id: 0, enclave: "/", namespace: "/cell4", name: "gw" }, 10, EntityKind::Publisher, "/cell4/chatter", "example_interfaces::msg::dds_::String_", "RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b", &Qos { reliability: Reliability::Reliable, durability: Durability::Volatile, history: History::KeepLast, depth: 7 }),
        "@ros2_lv/7/304b88ed8d0d24d941ca41005d509970/0/10/MP/%/%cell4/gw/%cell4%chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b/::,7:,:,:,,"
    );
    assert_eq!(data_key(7, "/cell4/chatter", "example_interfaces::msg::dds_::String_", "RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b"), "7/cell4/chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b");
    check_attachment("17000000000000000d83fc79dda2dc181066aaa0f0bdc395448269a8214ca82fea", "@ros2_lv/7/304b88ed8d0d24d941ca41005d509970/0/10/MP/%/%cell4/gw/%cell4%chatter/example_interfaces::msg::dds_::String_/RIHS01_5509d866a579951f2fc6c19577c32605ba16f308cae7b498341d79536d4eb06b/::,7:,:,:,,");
}
