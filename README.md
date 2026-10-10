# FastDiscord

**FastDiscord é um cliente Discord nativo** — construído em **Rust + egui**, sem
nenhum browser engine. Este é o port do [Vesktop](https://github.com/Vencord/Vesktop)
(originalmente Electron/TypeScript) para o padrão dos apps nativos
[spotifast](https://github.com/crmne/spotifast) e
[zapfast](https://github.com/crmne/zapfast): a UI inteira é desenhada com
[egui](https://github.com/emilk/egui) (via o fork mantido
[crmne/egui](https://github.com/crmne/egui), ligado por `[patch.crates-io]`),
e o Discord é falado nativamente — REST + Gateway sobre tokio.

> Projeto não oficial, sem qualquer afiliação com o Discord. Clientes de
> terceiros podem violar os Termos de Serviço do Discord — use por sua conta
> e risco. Mantém a licença **GPL-3.0-or-later** do Vesktop original.

## Recursos

- Cliente nativo em Rust + egui, com login por QR ou token.
- Servidores, canais, DMs, amigos e solicitações de amizade.
- Mensagens ao vivo, cache com atualização paginada após reconexão, busca no
  histórico carregado, links clicáveis, convites, emojis, GIFs e anexos.
- Avatares com presença e visualizador de imagens com zoom e salvamento.
- Voz com seleção de dispositivos, volumes de entrada/saída e por usuário,
  mute local, supressão de ruído, ganho automático, compressor e limitador.
- Cancelamento de eco pelo servidor de som no Linux e por WASAPI no Windows
  quando o sistema e o dispositivo oferecem AEC; a interface informa quando
  o Windows precisa usar captura sem cancelamento.
- Soundboard e DJ com fila, pausa e cancelamento; o DJ requer `yt-dlp` e `ffmpeg`.
- Compartilhamento de tela pelo FockyTV, sons do Discord, notificações de DMs e
  menções, configurações em abas e animações de hover, seleção e clique.
- Configurações persistentes e bandeja experimental (`--features tray`).

As notificações no Linux usam `notify-send`, `paplay` e o tema de sons
freedesktop. O upload básico de anexos aceita arquivos de até 10 MB. A busca
no chat consulta apenas mensagens já carregadas. Recursos ainda previstos,
como threads e plugins, estão em [docs/PORT.md](docs/PORT.md).

## Instalação

Baixe na [página de releases](https://github.com/FelipeMayerDev/FastDiscord/releases):
`FastDiscord-x86_64.AppImage` (Linux) ou `FastDiscord-windows-x86_64.exe`
(Windows, um único executável que se extrai em
`%LOCALAPPDATA%\FastDiscord\<versão>` na primeira execução). Um `.desktop` de
referência fica em [packaging/fastdiscord.desktop](packaging/fastdiscord.desktop).

## Build from Source

Você precisa do Rust (o `rust-toolchain.toml` fixa a versão — o `rustup`
instala sozinho) e das libs gráficas do seu desktop; no Linux, os pacotes de
runtime usuais do X11/Wayland. Nenhum header de GTK/WebKit é necessário.

```sh
git clone https://github.com/FelipeMayerDev/FastDiscord
cd FastDiscord

cargo run --release
# bandeja experimental:
cargo run --release --features tray
```

Dica: `cargo build --release` gera o binário em `target/release/fastdiscord`;
a primeira compilação resolve `Cargo.lock` automaticamente.

## Como obter o token

1. Abra `discord.com` no navegador e entre na sua conta;
2. Aperte `Ctrl+Shift+I` para abrir o DevTools;
3. Na aba **Console**, rode `localStorage.token`;
4. Copie o valor (entre aspas) e cole na tela de login do FastDiscord.

Também é possível passar direto: `fastdiscord --token SEU_TOKEN`.

## Onde ficam as configurações

`settings.json` no diretório de configuração do usuário
(Linux: `~/.config/fastdiscord/`; macOS: `~/Library/Application Support/app.FastDiscord.FastDiscord/`;
Windows: `%APPDATA%\FastDiscord\FastDiscord\config\`). O token hoje vive nesse arquivo — movê-lo para
o keyring do sistema é item do roadmap.

## Roadmap

Ver [docs/PORT.md](docs/PORT.md). Próximos passos naturais: adotar os crates
`fastframe-*` (bandeja, fontes, i18n, updates), gateway RESUME, keyring para o
token e uma história de plugins nativa.

## Créditos e licença

- [Vesktop](https://github.com/Vencord/Vesktop) por Vendicated e contribuidores
  — o app original, GPL-3.0-or-later; este fork portou a ideia para Rust.
- [crmne](https://github.com/crmne) — padrão de app nativo Rust+egui
  (spotifast/zapfast/fastframe) e fork do egui usado aqui.
- [egui](https://github.com/emilk/egui) por Emil Ernerfeldt e contribuidores.

Licença: [GPL-3.0-or-later](LICENSE), herdada do Vesktop original.
