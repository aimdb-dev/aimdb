//! Data keys and liveliness tokens.

use alloc::string::String;

use super::Qos;

/// A ROS 2 node's identity in liveliness tokens.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Node<'a> {
    pub domain: u32,
    /// The Zenoh session id, as Zenoh prints it.
    pub zid: &'a str,
    pub id: u32,
    /// `/` is the default enclave.
    pub enclave: &'a str,
    /// `/` or `/cell4`.
    pub namespace: &'a str,
    pub name: &'a str,
}

/// Whether an entity publishes or subscribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityKind {
    Publisher,
    Subscription,
}

/// A name as a single token chunk: every `/` becomes `%`, and an empty name
/// is `%`.
pub(crate) fn mangle(name: &str) -> String {
    if name.is_empty() {
        return String::from("%");
    }
    name.replace('/', "%")
}

/// `<domain>/<topic>/<type>/<hash>`, the topic without its slashes at either
/// end: `0/cell4/temperature/sensor_msgs::msg::dds_::Temperature_/RIHS01_…`.
pub(crate) fn data_key(domain: u32, topic: &str, type_name: &str, type_hash: &str) -> String {
    let topic = topic.trim_matches('/');
    alloc::format!("{domain}/{topic}/{type_name}/{type_hash}")
}

/// `@ros2_lv/<domain>/<zid>/<node id>/<node id>/NN/<enclave>/<namespace>/<name>`.
pub(crate) fn node_token(node: &Node<'_>) -> String {
    alloc::format!(
        "@ros2_lv/{}/{}/{}/{}/NN/{}/{}/{}",
        node.domain,
        node.zid,
        node.id,
        node.id,
        mangle(node.enclave),
        mangle(node.namespace),
        node.name
    )
}

/// `@ros2_lv/<domain>/<zid>/<node id>/<entity id>/MP|MS/<enclave>/<namespace>/<name>/<topic>/<type>/<hash>/<qos>`.
pub(crate) fn entity_token(
    node: &Node<'_>,
    entity_id: u32,
    kind: EntityKind,
    topic: &str,
    type_name: &str,
    type_hash: &str,
    qos: &Qos,
) -> String {
    let kind = match kind {
        EntityKind::Publisher => "MP",
        EntityKind::Subscription => "MS",
    };
    alloc::format!(
        "@ros2_lv/{}/{}/{}/{entity_id}/{kind}/{}/{}/{}/{}/{type_name}/{type_hash}/{}",
        node.domain,
        node.zid,
        node.id,
        mangle(node.enclave),
        mangle(node.namespace),
        node.name,
        mangle(topic),
        qos.encode()
    )
}
