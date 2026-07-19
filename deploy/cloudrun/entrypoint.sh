#!/bin/sh
exec opensnow start --config /etc/opensnow/opensnow.toml --http-port "${PORT:-8080}"
