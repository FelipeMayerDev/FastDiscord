#!/usr/bin/env bash
# Owned by the GoLiveBypass plugin; pkexec authenticates before this helper runs.
set -euo pipefail
export PATH=/usr/bin:/bin

fail() { echo "FastDiscord bypass: $*" >&2; exit 1; }
check=false
if [[ ${1:-} == --check ]]; then check=true; shift; fi
[[ $# -ge 1 ]] || fail 'informe o perfil.conf'
profile=$1
[[ -f $profile ]] || fail 'perfil não encontrado'

# Read only Interface fields; never execute profile hooks (PostUp etc.).
field() {
    awk -v key="$1" '
        /^[[:space:]]*\[/ { interface = ($0 ~ /^[[:space:]]*\[Interface\][[:space:]]*$/) }
        interface && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]*#.*/, "");
            gsub(/,/, " "); print
        }' "$profile"
}
addresses=$(field Address)
dns=$(field DNS)
[[ -n $addresses && -n $dns ]] || fail 'o perfil precisa de Address e DNS (endereços IP)'
for address in $addresses; do
    [[ $address =~ ^[0-9a-fA-F:.]+/[0-9]+$ ]] || fail 'Address inválido'
done
for server in $dns; do
    [[ $server =~ ^[0-9a-fA-F:.]+$ && $server == *[.:]* ]] || fail 'DNS deve conter apenas endereços IP'
done
$check && exit 0
[[ $# -ge 3 ]] || fail 'esperado: perfil.conf executável usuário [variáveis de desktop]'
binary=$2
user=$3
shift 3
[[ $EUID == 0 ]] || fail 'execute pelo FastDiscord com --wireguard'
[[ $user != root ]] || fail 'o aplicativo deve executar como usuário comum'
[[ -x $binary ]] || fail 'executável não encontrado'
for tool in ip wg wg-quick runuser; do
    command -v "$tool" >/dev/null || fail "dependência ausente: $tool (instale wireguard-tools e iproute2)"
done
for variable in "$@"; do
    case "$variable" in
        DISPLAY=*|WAYLAND_DISPLAY=*|XDG_RUNTIME_DIR=*|DBUS_SESSION_BUS_ADDRESS=*|XAUTHORITY=*|FASTDISCORD_READY_FILE=*) ;;
        *) fail 'variável de ambiente não permitida' ;;
    esac
done

namespace=fastdiscord-bypass-$$
interface=fdwg$$
config=$(mktemp -d /run/fastdiscord-wg.XXXXXX)
created=false
dns_created=false
host_interface=false
cleanup() {
    rm -rf "$config"
    if $created; then
        # Close every process still holding this private namespace before deletion.
        for pid in $(ip netns pids "$namespace"); do kill "$pid" 2>/dev/null || true; done
        ip netns delete "$namespace" || true
    fi
    if $host_interface; then ip link delete "$interface" 2>/dev/null || true; fi
    if $dns_created; then rm -rf "/etc/netns/$namespace"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cp "$profile" "$config/vpn.conf"
wg-quick strip "$config/vpn.conf" > "$config/stripped.conf"
ip netns add "$namespace"
created=true
ip link add "$interface" type wireguard
host_interface=true
# The UDP socket is born in the host namespace, so its endpoint keeps the normal route.
wg setconf "$interface" "$config/stripped.conf"
rm -rf "$config"
allowed=$(wg show "$interface" allowed-ips)
[[ $allowed == *'0.0.0.0/0'* ]] || fail 'AllowedIPs precisa incluir 0.0.0.0/0'
if [[ $addresses == *:* ]]; then
    [[ $allowed == *'::/0'* ]] || fail 'perfil com IPv6 precisa incluir ::/0 em AllowedIPs'
fi
ip link set "$interface" netns "$namespace"
host_interface=false
ip -n "$namespace" link set lo up
for address in $addresses; do ip -n "$namespace" address add "$address" dev "$interface"; done
ip -n "$namespace" link set "$interface" mtu 1280 up
ip -n "$namespace" route add default dev "$interface"
if [[ $addresses == *:* ]]; then ip -n "$namespace" -6 route add default dev "$interface"; fi
mkdir -p /etc/netns
mkdir "/etc/netns/$namespace"
dns_created=true
for server in $dns; do printf 'nameserver %s\n' "$server"; done > "/etc/netns/$namespace/resolv.conf"
echo 'FastDiscord: túnel isolado ativo; Go Live experimental. Feche pela bandeja para encerrar.'
ip netns exec "$namespace" runuser -u "$user" -- env "$@" FASTDISCORD_WIREGUARD=1 "$binary" --go-live
