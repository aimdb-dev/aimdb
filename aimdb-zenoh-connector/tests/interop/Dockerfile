# A ROS 2 distribution with rmw_zenoh, for tests/interop/ros2_interop.sh.
# geometry_msgs is for the ros2_gateway example's Twist on /cmd_vel.
ARG DISTRO=lyrical
FROM ros:${DISTRO}
ARG DISTRO
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ros-${DISTRO}-rmw-zenoh-cpp \
      ros-${DISTRO}-geometry-msgs \
 && rm -rf /var/lib/apt/lists/*
ENV RMW_IMPLEMENTATION=rmw_zenoh_cpp
