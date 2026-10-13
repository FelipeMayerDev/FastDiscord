# Plugin GoLiveBypass (Linux)

O plugin nativo do FastDiscord adapta a ideia do
[GoLiveBypass de bezumiya](https://github.com/bezumiya/GoLiveBypass): um túnel
WireGuard exclusivo para o processo do cliente. O plugin original é para
Vencord/Equicord/Vesktop; o FastDiscord é Rust/egui e usa uma implementação
nativa, com código isolado em `src/plugins/golive_bypass/`.

Em **Configurações → Plugins → GoLiveBypass → Configurar**, clique em
**Importar .conf** e selecione um perfil WireGuard privado com servidor fora
do Brasil. Depois marque **Ativado** e clique em **Reiniciar para aplicar o
bypass**. A senha solicitada é a do sistema. A sessão anterior só encerra
quando a nova janela estiver pronta; cancelar a autorização mantém a anterior.

O perfil é validado e copiado para o diretório privado do plugin, com pasta
0700 e arquivo 0600 no Linux. Somente o caminho entra nas configurações JSON;
a chave privada permanece no arquivo. A importação não envia o arquivo a um
servidor. Um perfil inválido não substitui a última cópia válida.

Instale `wireguard-tools`, `iproute2` e `polkit`. O perfil precisa de
`PrivateKey`, `Address`, `DNS` com endereços IP, um peer com `PublicKey` e
`AllowedIPs = 0.0.0.0/0` (também `::/0` para IPv6). Hooks como `PostUp`
não são executados. A saída do servidor VPN deve estar fora do Brasil.

O túnel cobre REST, gateway, voz e vídeo do FastDiscord; outros aplicativos
continuam na rede normal. Enquanto estiver ativo, o plugin escolhe o Go Live
nativo para transmitir e assistir e sobrepõe a seleção do FockyTV, preservando
as preferências desse plugin. O título da live identifica o GoLiveBypass.
A alteração de ativação só muda a rede depois de reiniciar.

Para sair do túnel, desmarque GoLiveBypass e reinicie. O reinício usa o
`systemd --user` para criar a nova sessão fora da namespace antiga. Fechar
apenas a janela pode manter o processo na bandeja; use **Sair** para encerrar.
SIGKILL ou desligamento abrupto pode deixar uma namespace até reiniciar.

```sh
cargo build --release
# Importa o perfil e salva a ativação do plugin:
./target/release/fastdiscord --wireguard /caminho/privado/vpn.conf
# Os próximos inícios usam as preferências dos plugins:
./target/release/fastdiscord
# Recuperação/comparação, sem ativação automática do bypass:
./target/release/fastdiscord --no-bypass --go-live
```

Esta versão configura o bypass por importação de `.conf`. Login Proton
integrado e suporte Windows não estão implementados. A câmera nativa do
Discord continua sem implementação; o plugin FockyTV mantém sua câmera.
A reprodução Go Live foi testada com o túnel; a publicação nativa precisa
ser retestada com espectadores.

Validação sem privilégios: `bash tests/golive-bypass.sh` e
`cargo test --lib plugins::`.
