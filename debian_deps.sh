#!/bin/bash

apt update -qq && apt install -yqq \
build-essential \
pkg-config \
libgtk-4-dev \
libgtk4-layer-shell-dev \
ca-certificates \
git \
wget \
curl \
unzip \
libglib2.0-dev \
gobject-introspection \
libgirepository1.0-dev \
nodejs npm 

