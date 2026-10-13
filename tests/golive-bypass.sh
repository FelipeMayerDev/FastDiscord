#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
directory=$(mktemp -d)
trap 'rm -rf "$directory"' EXIT
profile=$directory/test.conf
cat > "$profile" <<'EOF'
[Interface]
Address = 10.0.0.2/32, fd00::2/128
DNS = 1.1.1.1, 2606:4700:4700::1111
PostUp = exit 99
[Peer]
Endpoint = example.com:51820
AllowedIPs = 0.0.0.0/0, ::/0
EOF
bash src/plugins/golive_bypass/linux.sh --check "$profile"
reject() {
    if bash src/plugins/golive_bypass/linux.sh --check "$profile" 2>/dev/null; then
        echo "aceitou perfil inválido: $1" >&2
        exit 1
    fi
}
sed -i 's/DNS = .*/DNS = $(touch SHOULD_NOT_EXIST)/' "$profile"
reject 'comando em DNS'
[[ ! -e SHOULD_NOT_EXIST ]]
sed -i 's/DNS = .*/DNS = resolver.example.com/' "$profile"
reject 'hostname em DNS'
sed -i '/DNS =/d' "$profile"
reject 'DNS ausente'
sed -i '/Address =/d' "$profile"
reject 'Address ausente'
printf '[Peer]\nAddress = 10.0.0.2/32\nDNS = 1.1.1.1\n' > "$profile"
reject 'campos na seção Peer'
echo 'Go Live bypass: validação de perfis OK'
