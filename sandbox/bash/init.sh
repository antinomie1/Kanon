#!/bin/sh
# Only this trusted bootstrap runs with setup capabilities. User code starts after they are dropped.
set -eu
if [ "$#" -ne 2 ]; then
    echo 'Invalid sandbox startup contract' >&2
    exit 125
fi
sandbox_uid=$1
sandbox_gid=$2
case "$sandbox_uid:$sandbox_gid" in
    *[!0-9:]*) echo 'Invalid sandbox startup values' >&2; exit 125 ;;
esac
if [ "$sandbox_uid" -eq 0 ]; then
    echo 'Sandbox user must not be root' >&2
    exit 125
fi

# Allow public Internet access, including Docker DNS on the container loopback interface. Block
# host/LAN/metadata routes so enabled networking cannot reach local management APIs or cloud keys.
/usr/sbin/iptables -A INPUT -i lo -j ACCEPT
/usr/sbin/iptables -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
/usr/sbin/iptables -A INPUT -j REJECT
/usr/sbin/iptables -A OUTPUT -o lo -j ACCEPT
/usr/sbin/iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
# The default Docker bridge can use a private DNS resolver instead of 127.0.0.11. Permit only
# its DNS port; this does not grant access to other services on the resolver/host address.
while read -r sandbox_key sandbox_dns sandbox_rest; do
    [ "$sandbox_key" = nameserver ] || continue
    case "$sandbox_dns" in
        *:*) continue ;;
        ''|*[!0-9.]*) echo 'Invalid sandbox DNS configuration' >&2; exit 125 ;;
    esac
    /usr/sbin/iptables -A OUTPUT -p udp -d "$sandbox_dns" --dport 53 -j ACCEPT
    /usr/sbin/iptables -A OUTPUT -p tcp -d "$sandbox_dns" --dport 53 -j ACCEPT
done < /etc/resolv.conf
for sandbox_range in 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12 192.168.0.0/16 224.0.0.0/4 240.0.0.0/4; do
    /usr/sbin/iptables -A OUTPUT -d "$sandbox_range" -j REJECT
done

# The marker is created by root after network setup. The persistent container stays alive;
# each subsequent command uses the trusted exec helper to drop identity and capabilities again.
/usr/bin/touch /tmp/kanon-ready
/usr/bin/chmod 0444 /tmp/kanon-ready
exec /usr/bin/setpriv \
    --reuid="$sandbox_uid" --regid="$sandbox_gid" --clear-groups \
    --bounding-set=-all --inh-caps=-all --ambient-caps=-all --no-new-privs \
    /usr/bin/sleep infinity
