#!/usr/bin/env bash
# ros2:// against a real ROS 2 distribution: rmw_zenohd as the router, the
# ros2 CLI as the other side, the ros2_talker and ros2_listener examples as
# AimDB.
#
#   ros2_interop.sh <distro>
#
# Needs Docker with host networking and the examples built:
#   cargo build -p aimdb-zenoh-connector --features std --examples
# IMAGE overrides the image (built from this directory's Dockerfile
# otherwise); EXAMPLES overrides the directory of the example binaries.
set -euo pipefail

DISTRO=${1:?usage: ros2_interop.sh <distro>}
HERE=$(cd "$(dirname "$0")" && pwd)
EXAMPLES=${EXAMPLES:-target/debug/examples}
AIMDB=${AIMDB:-target/debug/aimdb}
IMAGE=${IMAGE:-aimdb-ros2-interop:$DISTRO}
PREFIX=aimdb-interop-$DISTRO-$$
PIDS=()

cleanup() {
    for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
    for c in $(docker ps -aq --filter "name=$PREFIX"); do
        docker stop "$c" >/dev/null 2>&1 || true
        docker container rm "$c" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT

fail() {
    echo "FAIL ($DISTRO): $*" >&2
    exit 1
}

# Runs a ros2 CLI command in a container and prints its output. Detached
# and read back with `docker logs`, which works on every Docker setup.
ros() {
    local name=$PREFIX-cli-$RANDOM
    docker run -d --name "$name" --network host \
        -e ROS_DOMAIN_ID="${ROS_DOMAIN_ID:-0}" "$IMAGE" "$@" >/dev/null
    docker wait "$name" >/dev/null
    docker logs "$name" 2>&1
    docker container rm "$name" >/dev/null
}

# Retries `ros ...` until its output matches the pattern: ROS discovery
# takes a few seconds after a node appears.
ros_until() {
    local pattern=$1 out
    shift
    for _ in $(seq 1 10); do
        out=$(ros "$@") || true
        if grep -qE "$pattern" <<<"$out"; then
            printf '%s\n' "$out"
            return 0
        fi
        sleep 2
    done
    printf '%s\n' "$out" >&2
    return 1
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "Building $IMAGE"
    docker build -q --build-arg DISTRO="$DISTRO" -t "$IMAGE" "$HERE" >/dev/null
fi

echo "Starting rmw_zenohd ($DISTRO)"
docker run -d --name "$PREFIX-router" --network host "$IMAGE" \
    ros2 run rmw_zenoh_cpp rmw_zenohd >/dev/null
for _ in $(seq 1 30); do
    (exec 3<>/dev/tcp/127.0.0.1/7447) 2>/dev/null && break
    sleep 1
done
(exec 3<>/dev/tcp/127.0.0.1/7447) 2>/dev/null || fail "rmw_zenohd did not listen on 7447"

# --- AimDB publishes; ROS sees the node, the topic and the values.
"$EXAMPLES/ros2_talker" >/dev/null 2>&1 &
PIDS+=($!)

ros_until '^/aimdb/talker$' ros2 node list >/dev/null \
    || fail "ros2 node list does not show /aimdb/talker"
info=$(ros_until 'Publisher count: 1' ros2 topic info -v /aimdb_chatter) \
    || fail "ros2 topic info -v does not show the publisher"
for expected in \
    'Type: std_msgs/msg/String' \
    'Node name: talker' \
    'Node namespace: /aimdb' \
    'Topic type hash: RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18' \
    'Endpoint type: PUBLISHER' \
    'GID: [0-9a-f.]{47}' \
    'Reliability: RELIABLE' \
    'History \(Depth\): KEEP_LAST \(10\)' \
    'Durability: VOLATILE'; do
    grep -qE "$expected" <<<"$info" || fail "ros2 topic info -v lacks '$expected':
$info"
done
ros_until '^data: aimdb [0-9]+' timeout 20 ros2 topic echo --once /aimdb_chatter >/dev/null \
    || fail "ros2 topic echo received nothing from /aimdb_chatter"
echo "ok: publisher (node list, topic info -v, topic echo)"

# --- ROS publishes; AimDB receives every message.
listener_log=$(mktemp)
"$EXAMPLES/ros2_listener" >"$listener_log" 2>&1 &
PIDS+=($!)
ros_until 'Subscription count: 1' ros2 topic info -v /aimdb_cmd >/dev/null \
    || fail "ros2 topic info -v does not show the subscription"
ros timeout 30 ros2 topic pub --times 3 -r 2 /aimdb_cmd std_msgs/msg/String \
    "{data: hello from $DISTRO}" >/dev/null
for _ in $(seq 1 10); do
    [[ $(grep -c "received: hello from $DISTRO" "$listener_log") -ge 3 ]] && break
    sleep 1
done
received=$(grep -c "received: hello from $DISTRO" "$listener_log" || true)
rm -f "$listener_log"
[[ $received -eq 3 ]] || fail "the listener received $received of 3 messages"
echo "ok: subscription (topic info -v, topic pub, 3 of 3 received)"

# --- The gateway (examples/ros2_gateway.md): ROS publishes a Twist on
# /cmd_vel in domain 42; AimDB stores it, republishes it as JSON on plain
# Zenoh and serves it over AimX.
export ROS_DOMAIN_ID=42
socket=$(mktemp -u /tmp/aimdb-interop-XXXXXX.sock)
gateway_log=$(mktemp)
zenoh_log=$(mktemp)
watch_log=$(mktemp)
AIMX_SOCKET=$socket "$EXAMPLES/ros2_gateway" >"$gateway_log" 2>&1 &
PIDS+=($!)
"$EXAMPLES/zenoh_sub" 'telemetry/**' >"$zenoh_log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 30); do
    [[ -S $socket ]] && grep -q '^zenoh_sub:' "$zenoh_log" && break
    sleep 1
done
[[ -S $socket ]] || fail "the gateway did not open $socket"
grep -q '^zenoh_sub:' "$zenoh_log" || fail "zenoh_sub did not subscribe:
$(cat "$zenoh_log")"
"$AIMDB" --connect "unix://$socket" watch robot.cmd_vel --format json --new \
    --count 3 --timeout 60 >"$watch_log" 2>&1 &
watch_pid=$!
PIDS+=($watch_pid)

ros_until 'Subscription count: 1' ros2 topic info -v /cmd_vel >/dev/null \
    || fail "ros2 topic info -v does not show the gateway's subscription"
ros timeout 30 ros2 topic pub --times 3 -r 2 /cmd_vel geometry_msgs/msg/Twist \
    "{linear: {x: 0.5}, angular: {z: 0.3}}" >/dev/null

json='{"linear":{"x":0.5,"y":0.0,"z":0.0},"angular":{"x":0.0,"y":0.0,"z":0.3}}'
for _ in $(seq 1 10); do
    [[ $(grep -cxF "telemetry/cmd_vel: $json" "$zenoh_log") -ge 3 ]] && break
    sleep 1
done
republished=$(grep -cxF "telemetry/cmd_vel: $json" "$zenoh_log" || true)
[[ $republished -eq 3 ]] || fail "zenoh_sub received $republished of 3 JSON payloads:
$(cat "$zenoh_log")"
wait "$watch_pid" || fail "aimdb watch did not see 3 updates:
$(cat "$watch_log")"
watched=$(grep -c '"value":{"angular":{"x":0.0,"y":0.0,"z":0.3},"linear":{"x":0.5,"y":0.0,"z":0.0}}' "$watch_log" || true)
[[ $watched -eq 3 ]] || fail "aimdb watch printed $watched of 3 Twists:
$(cat "$watch_log")"
rm -f "$gateway_log" "$zenoh_log" "$watch_log"
unset ROS_DOMAIN_ID
echo "ok: gateway (ros2 topic pub, 3 of 3 republished as JSON, aimdb watch)"

echo "PASS ($DISTRO)"
