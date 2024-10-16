#!/bin/sh

case "$(uname)" in
    Darwin)
        iface=en0
        ;;
    Linux)
        iface="$(ip addr|grep UP|cut -d: -f2|grep -v lo|head -n 1|xargs)"
        ;;
    MINGW*)
        iface="$(netsh interface ip show addresses|grep Configuration|awk '{print $4}'|head -n 1|xargs)"
        ;;
    *)
        iface="lo"
        ;;
esac

cat > ci_config.toml << EOF
# DDNS configuration for fritz.gtskhost.systems

[core]
url = "https://api.dev.name.com"
username = "gtskhadadze83@gmail.com-test"
key = "6a002be3412c21b8a9b67ff1820d1e48de476d16"

[[records]]
host = "ddns.gtskhost.system"
zone = "gtskhost.system"
type = "A"
ttl = 300
method = "local"
interface = "en0"
EOF
