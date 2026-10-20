# Loaded by the `cmake` crate for MSVC targets (see config.toml). Opus picks its
# MSVC runtime itself, ignoring the flags from Cargo, so build it with the static
# runtime when Rust links it statically (dist builds with +crt-static).
if("$ENV{CARGO_CFG_TARGET_FEATURE}" MATCHES "(^|,)crt-static(,|$)")
  set(OPUS_STATIC_RUNTIME ON CACHE BOOL "" FORCE)
endif()
