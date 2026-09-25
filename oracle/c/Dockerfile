# The oracle's compiler: a 32-bit Debian with gcc, so id's 1996 C (which
# assumes 32-bit pointers in places) builds as the i386 program it was.
FROM i386/debian:bookworm-slim@sha256:eec2e08d4c6ae5e701c735ef112bfd06730f441e78be405be03ddbec5a7143d2
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*
