# A ROS 2 distribution with rmw_zenoh, for tests/interop/ros2_interop.sh.
ARG DISTRO=lyrical
FROM ros:${DISTRO}
ARG DISTRO
RUN apt-get update \
 && apt-get install -y --no-install-recommends ros-${DISTRO}-rmw-zenoh-cpp \
 && rm -rf /var/lib/apt/lists/*
ENV RMW_IMPLEMENTATION=rmw_zenoh_cpp
