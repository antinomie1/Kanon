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
COPY exec.sh /usr/local/libexec/kanon-sandbox-exec
RUN chmod 0555 /usr/local/libexec/kanon-sandbox-init /usr/local/libexec/kanon-sandbox-exec
LABEL org.kanon.bash-sandbox.version="2"
ENV HOME=/workspace/.home LANG=C.UTF-8 PYTHONDONTWRITEBYTECODE=1 PIP_NO_CACHE_DIR=1
ENV PATH=/workspace/.home/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    NPM_CONFIG_PREFIX=/workspace/.home/.local
WORKDIR /workspace
ENTRYPOINT ["/usr/local/libexec/kanon-sandbox-init"]
