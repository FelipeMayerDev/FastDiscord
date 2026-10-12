# Go Live bypass experimental (Linux)

Nesta branch, `--wireguard` inicia todo o FastDiscord em um namespace de
rede com WireGuard. O túnel usa a conexão normal do computador para chegar
ao servidor VPN; os outros aplicativos continuam na rede normal. REST,
gateway, voz e vídeo do FastDiscord usam o mesmo túnel. Sem o túnel não há
rota alternativa dentro do namespace.

Instale `wireguard-tools`, `iproute2` e `polkit` pelos pacotes da sua distro.
Use um perfil WireGuard **privado**, com servidor fora do Brasil, `Address`,
`DNS` contendo IPs e `AllowedIPs = 0.0.0.0/0` (também `::/0` para IPv6).
O perfil precisa ter extensão `.conf`. Hooks como `PostUp` não são executados.
As chaves não são copiadas para as configurações do FastDiscord.

Em **Configurações → Tela e transmissão**, informe o caminho do perfil e
clique em **Abrir sessão com bypass**. Após abrir a nova janela, saia da
sessão original pela bandeja. Se cancelar a autorização, a original permanece
aberta. O caminho do perfil não é salvo.

```sh
cargo build
./target/debug/fastdiscord --wireguard /caminho/privado/vpn.conf
```

A autenticação administrativa é feita pelo diálogo do `pkexec`; o app
executa como seu usuário, não como root. O helper remove o túnel ao sair
do aplicativo. Fechar só a janela pode manter o app na bandeja: use **Sair**.
O helper deve continuar executando enquanto o aplicativo estiver aberto;
SIGKILL ou desligamento abrupto pode deixar um namespace até reiniciar.

O comando seleciona o backend Go Live, mas não salva essa seleção ao iniciar.
Alterações posteriores nas configurações podem salvá-la normalmente.
Para comparar sem VPN, use `./target/debug/fastdiscord --go-live`.
Para voltar ao backend padrão, marque FockyTV em Tela e transmissão.

Teste com um espectador no Discord oficial: entrar na chamada, compartilhar
uma tela e verificar se o espectador recebe vídeo. Repita sem o túnel.
O problema documentado em SCREENSHARE.md (vídeo não encaminhado pelo SFU)
ainda não foi confirmado como bloqueio regional. A câmera nativa do Discord
continua sem implementação. Não há suporte Windows nesta primeira versão,
nem integração automática com Proton ou perfis públicos compartilhados.

Validação sem privilégios: `bash tests/golive-bypass.sh`.
