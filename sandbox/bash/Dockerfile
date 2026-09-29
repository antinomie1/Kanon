FROM node:24-bookworm-slim AS node
FROM python:3.12-slim-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash coreutils git ripgrep iptables util-linux ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && bash --version && python3 --version && node --version
COPY init.sh /usr/local/libexec/kanon-sandbox-init
RUN chmod 0555 /usr/local/libexec/kanon-sandbox-init
LABEL org.kanon.bash-sandbox.version="1"
ENV HOME=/tmp LANG=C.UTF-8 PYTHONDONTWRITEBYTECODE=1 PIP_NO_CACHE_DIR=1
WORKDIR /workspace
ENTRYPOINT ["/usr/local/libexec/kanon-sandbox-init"]
