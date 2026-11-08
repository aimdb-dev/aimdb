//! Drives turtlesim's turtle with goals set over AimX, for example by Claude
//! through `aimdb-mcp`. The caller says where to go; this process decides how
//! to move.
//!
//! ```text
//! turtle.goal  "Stop" | {"GoTo": {"x": 8.0, "y": 8.0}}    writable over AimX
//! /turtle1/pose ──▶ turtle.pose                           read-only
//! turtle.goal + turtle.pose ──controller──▶ turtle.cmd_vel ──▶ /turtle1/cmd_vel
//! ```
//!
//! `turtle.cmd_vel` is the output of a transform, so AimDB refuses remote
//! writes to it: the only way to move the turtle is a goal, and the speed
//! limits live in the controller.
//!
//! The router is `ZENOH_ENDPOINT` (default `tcp/127.0.0.1:7447`), the domain
//! `ROS_DOMAIN_ID` (default 0), and AimX listens on `AIMX_SOCKET` (default
//! `/tmp/aimdb-turtlesim.sock`). The pose is `turtlesim/msg/Pose` on Jazzy
//! and `turtlesim_msgs/msg/Pose` on later distros, chosen by `ROS_DISTRO`
//! (default Jazzy).
//!
//! ```text
//! ros2 run turtlesim turtlesim_node
//! cargo run -p aimdb-zenoh-connector --features std --example turtlesim_mcp
//! aimdb record set turtle.goal '{"GoTo": {"x": 8.0, "y": 8.0}}'
//! aimdb watch turtle.cmd_vel
//! ```
//!
//! Walkthrough, including Claude through `aimdb-mcp`: `docs/ros2-walkthrough.md`.

use std::f32::consts::{FRAC_PI_4, PI};
use std::fmt::Debug;
use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::{LinkableRegistrarExt, RosMessage};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// What the controller needs from a pose, whichever package it comes from.
trait TurtlePose:
    RosMessage + Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static
{
    fn position(&self) -> (f32, f32, f32);
}

/// turtlesim's pose, as Jazzy publishes it.
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "turtlesim/msg/Pose",
    hash = "RIHS01_739beba26bcba6920404ba722b7b8321348512f92ea5be235c47251940dd8aa9"
)]
struct JazzyPose {
    x: f32,
    y: f32,
    theta: f32,
    linear_velocity: f32,
    angular_velocity: f32,
}

/// The same message after it moved to `turtlesim_msgs` (Lyrical): a new type
/// name, so a new hash.
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "turtlesim_msgs/msg/Pose",
    hash = "RIHS01_37b07ace09a0ed2c5b774b1410dec972dfd1d292de5656ebc2b075ac646e09c8"
)]
struct Pose {
    x: f32,
    y: f32,
    theta: f32,
    linear_velocity: f32,
    angular_velocity: f32,
}

impl TurtlePose for JazzyPose {
    fn position(&self) -> (f32, f32, f32) {
        (self.x, self.y, self.theta)
    }
}

impl TurtlePose for Pose {
    fn position(&self) -> (f32, f32, f32) {
        (self.x, self.y, self.theta)
    }
}

/// `geometry_msgs/msg/Vector3`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "geometry_msgs/msg/Vector3",
    hash = "RIHS01_cc12fe83e4c02719f1ce8070bfd14aecd40f75a96696a67a2a1f37f7dbb0765d"
)]
struct Vector3 {
    x: f64,
    y: f64,
    z: f64,
}

/// `geometry_msgs/msg/Twist`, with the hash Jazzy and Lyrical publish.
#[derive(Clone, Debug, Default, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "geometry_msgs/msg/Twist",
    hash = "RIHS01_9c45bf16fe0983d80e3cfe750d6835843d265a9a6c46bd2e609fcddde6fb8d2a"
)]
struct Twist {
    linear: Vector3,
    angular: Vector3,
}

/// What the caller wants. Not a ROS type: it never leaves AimDB.
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Goal {
    Stop,
    GoTo { x: f32, y: f32 },
}

/// Fastest forward speed, in turtlesim units per second.
const MAX_LINEAR: f32 = 2.0;
/// Fastest turn, in radians per second.
const MAX_ANGULAR: f32 = 2.0;
/// Close enough to the target.
const TOLERANCE: f32 = 0.05;
/// Targets stay inside turtlesim's 11 × 11 arena, away from the walls.
const ARENA: (f32, f32) = (0.5, 10.5);

/// One controller step: the command that moves `pose` toward `goal`, or
/// `None` once the turtle is there or told to stop.
fn step(goal: &Goal, (px, py, theta): (f32, f32, f32)) -> Option<Twist> {
    let Goal::GoTo { x, y } = *goal else {
        return None;
    };
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let (dx, dy) = (
        x.clamp(ARENA.0, ARENA.1) - px,
        y.clamp(ARENA.0, ARENA.1) - py,
    );
    let distance = dx.hypot(dy);
    if distance < TOLERANCE {
        return None;
    }
    // Heading error, wrapped to (-π, π].
    let mut error = dy.atan2(dx) - theta;
    while error > PI {
        error -= 2.0 * PI;
    }
    while error <= -PI {
        error += 2.0 * PI;
    }
    // Turn toward the target; drive only when roughly facing it.
    let angular = (4.0 * error).clamp(-MAX_ANGULAR, MAX_ANGULAR);
    let linear = if error.abs() < FRAC_PI_4 {
        (1.5 * distance).min(MAX_LINEAR) * error.cos()
    } else {
        0.0
    };
    Some(Twist {
        linear: Vector3 {
            x: linear.into(),
            ..Vector3::default()
        },
        angular: Vector3 {
            z: angular.into(),
            ..Vector3::default()
        },
    })
}

#[tokio::main]
async fn main() {
    // turtlesim's messages moved to `turtlesim_msgs` after Jazzy. A wrong type
    // fails silently, so the choice is printed.
    match std::env::var("ROS_DISTRO").as_deref() {
        Ok("jazzy") | Err(_) => run::<JazzyPose>().await,
        Ok(_) => run::<Pose>().await,
    }
}

async fn run<P: TurtlePose>() {
    let endpoint = std::env::var("ZENOH_ENDPOINT").unwrap_or_else(|_| "tcp/127.0.0.1:7447".into());
    let socket =
        std::env::var("AIMX_SOCKET").unwrap_or_else(|_| "/tmp/aimdb-turtlesim.sock".into());

    let ros2 = Ros2Connector::new(
        endpoint.as_str(),
        Ros2Node::new("turtle_pilot").namespace("/aimdb"),
    )
    .register::<P>()
    .register::<Twist>();

    // AimX: every record is readable, only the goal is writable.
    let _ = std::fs::remove_file(&socket);
    let aimx = UdsServer::from_config(
        AimxConfig::uds_default()
            .socket_path(socket.as_str())
            .security_policy(SecurityPolicy::read_write().with_writable_key("turtle.goal")),
    );

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2)
        .with_connector(aimx);
    builder.configure::<Goal>("turtle.goal", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    builder.configure::<P>("turtle.pose", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .linked_from("ros2://turtle1/pose");
    });
    // Only the newest velocity command matters.
    builder.configure::<Twist>("turtle.cmd_vel", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .transform_join(|join| {
                join.input::<Goal>("turtle.goal")
                    .input::<P>("turtle.pose")
                    .on_triggers(|mut rx, producer| async move {
                        let mut goal = Goal::Stop;
                        let mut pose = None;
                        let mut moving = false;
                        while let Ok(trigger) = rx.recv().await {
                            if let Some(g) = trigger.as_input::<Goal>() {
                                println!("goal: {g:?}");
                                goal = g.clone();
                            } else if let Some(p) = trigger.as_input::<P>() {
                                pose = Some(p.position());
                            }
                            let Some(pose) = pose else { continue };
                            match step(&goal, pose) {
                                Some(twist) => {
                                    moving = true;
                                    producer.produce(twist);
                                }
                                // One zero command on arrival or Stop, then
                                // quiet until the next goal.
                                None if moving => {
                                    moving = false;
                                    println!("stopped at ({:.2}, {:.2})", pose.0, pose.1);
                                    producer.produce(Twist::default());
                                }
                                None => {}
                            }
                        }
                    })
            })
            .linked_to("ros2://turtle1/cmd_vel");
    });
    let (_db, runner) = builder.build().await.expect("build");
    println!(
        "turtlesim_mcp: /turtle1/pose ({}) -> controller -> /turtle1/cmd_vel",
        P::ROS_TYPE_NAME
    );
    println!("turtlesim_mcp: router {endpoint}, AimX unix://{socket} (turtle.goal writable)");
    runner.run().await;
}
