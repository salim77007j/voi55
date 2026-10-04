#!/bin/sh
# Sandbox environment for building/running Micro-Vocal Lab without root.
# Source this file (`. scripts/env.sh`) before cargo commands.
#
# On a normal Linux machine with `libasound2-dev` installed, this file is
# unnecessary — cpal/alsa-sys find the system ALSA via pkg-config.

# Rust toolchain (rustup installs to ~/.cargo)
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) PATH="$HOME/.cargo/bin:$PATH"; export PATH ;;
esac

# Local ALSA 1.2.14 prefix (extracted from Debian 13 .debs by
# scripts/setup-alsa-prefix.sh) — provides the headers, libasound.so and
# the patched alsa.pc that alsa-sys links against.
ALSA_PREFIX="/home/z/my-project/.alsa-prefix"
if [ -d "$ALSA_PREFIX" ]; then
  PKG_CONFIG_PATH="$ALSA_PREFIX/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  LD_LIBRARY_PATH="$ALSA_PREFIX/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export PKG_CONFIG_PATH LD_LIBRARY_PATH
fi
