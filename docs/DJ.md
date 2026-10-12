# DJ compartilhado

O DJ é uma sessão do FastDiscord por **servidor + canal de voz**, sem bot.
Todos os participantes com a mesma configuração de DJ recebem a mesma
playlist, posição e controles. A música é reproduzida localmente: não sai
pelo microfone e não é ouvida no Discord oficial ou em versões antigas do app.

## Usar no app

1. O endereço padrão é `wss://dj.felipemayer.com.br`; a chave padrão vem de
   `FASTDISCORD_DJ_KEY` no momento do build. Para outro relay, altere em
   **Configurações → Voz e áudio → DJ compartilhado** e feche as configurações
   para aplicar.
2. Entre na sala de voz e clique no botão de música/DJ.
3. Cole um link HTTPS do YouTube para adicionar diretamente, ou pesquise pelo
   nome para ver até dez resultados com título e duração. Escolha **Adicionar**
   no resultado desejado. A duração é conferida antes de compartilhar o vídeo.
   Repita para montar a playlist.
4. O accordion aparece acima de **Voz conectada** para todos os participantes.
   Tem **voltar**, **play/pause**, **próxima**, **parar** e **playlist**.
5. O slider **Volume** afeta apenas você; o deafen também silencia o DJ.

Todos podem adicionar e controlar; o relay ordena os comandos recebidos.
Voltar reinicia a música se ela já passou de três segundos, ou volta à anterior.
Parar encerra a reprodução para todos e mantém a playlist para retomar com play.
Sair da voz encerra apenas o player local; a sessão continua para quem ficou.
Entrar depois ou reconectar recupera a música e a posição atual. A sessão é
apagada quando o último participante desconecta, ou quando o relay reinicia.

## Executar o relay

O projeto inclui `fastdiscord-dj-server`. Nenhum token do Discord é necessário.
O relay só troca metadados e comandos; não baixa nem armazena áudio.

```sh
cargo build --release --bin fastdiscord-dj-server
# Configure uma chave aleatória de pelo menos 16 caracteres no ambiente.
export FASTDISCORD_DJ_KEY='substitua-por-uma-chave-aleatoria'
# O padrão é loopback, apropriado para um proxy HTTPS na mesma máquina.
export FASTDISCORD_DJ_BIND='127.0.0.1:8787'
./target/release/fastdiscord-dj-server
```

Para experimentar na mesma máquina, use `ws://127.0.0.1:8787` no app e a mesma
chave. O modelo de Docker Compose está em [packaging/dj](../packaging/dj/README.md).
Ele recebe o binário compilado, mantém a chave fora da imagem e publica apenas
a porta de loopback para o proxy ou túnel.

Para usuários em outras máquinas, hospede o relay atrás de um proxy TLS
com suporte a WebSocket, usando `wss://dj.seudominio.com`. O app recusa `ws://`
fora de localhost e não aceita credenciais na URL. O endereço público precisa
ser configurado por quem hospedar o serviço; não existe relay público padrão.

Exemplo de proxy com Caddy, após apontar o domínio para a máquina:

```caddyfile
dj.seudominio.com {
    reverse_proxy 127.0.0.1:8787
}
```

A chave autoriza acesso ao relay inteiro, incluindo todas as salas. Compartilhe
somente com participantes de confiança. O relay não autentica contas nem
valida presença no Discord: o app escolhe a sala conforme sua conexão de voz.
Para um serviço público com usuários desconhecidos, é necessário adicionar
identidade/autorização por sala antes de distribuir a chave. Não exponha a
porta sem TLS nem reutilize o token do Discord como chave.

## Sincronização e limites

Cada app baixa a música inteira em uma pasta temporária local e confirma que
consegue decodificar o áudio. O relay aguarda as confirmações dos participantes
e programa o início três segundos adiante; o relógio não avança durante o download.
Cada app estima o relógio do relay pelo tempo de ida e volta e acompanha a posição.
Quem entra depois também baixa completamente e então busca a posição atual no
arquivo local. Falha de um participante não impede os demais de tocar.

Os temporários são removidos no fim, ao parar, trocar a música, sair da sala ou
encerrar normalmente o app. Nenhum áudio é salvo no relay. Encerramento forçado
pode deixar arquivos na pasta temporária até a limpeza do sistema. Para evitar
ocupar todo o disco, o download tem limite de 512 MiB e cinco minutos.

Isso reduz diferenças de posição, mas não garante saída exatamente simultânea:
rede, download e buffers do dispositivo de áudio variam. Enquanto o áudio
carrega, o app mostra **Baixando: X/Y prontos**. Um vídeo indisponível
ou bloqueado para um participante produz erro local, sem parar os demais.
Não há votação nem eleição de um app anfitrião: o relay mantém a sessão mesmo
que quem adicionou a música saia.

Por enquanto, são aceitos vídeos do YouTube com duração conhecida de até 24h;
lives não são aceitas. Todos os participantes precisam de `yt-dlp` e `ffmpeg`
(mais Deno para suporte completo ao YouTube). O pacote Windows já inclui os
auxiliares. Não são transmitidos cookies ou credenciais do YouTube.

O protocolo v2 limita mensagens a 1 MiB e a playlist a aproximadamente 512 KiB
de metadados, com erro explícito se ultrapassar esse tamanho. O relay limita
conexões a 256, controles a 20 por segundo por conexão e remove clientes sem
heartbeat por 15 segundos. Filas e sessões ficam apenas na memória.

Referências: [yt-dlp](https://github.com/yt-dlp/yt-dlp) e
[tokio-tungstenite](https://docs.rs/tokio-tungstenite/0.24.0/tokio_tungstenite/).

O protocolo v2 exige atualizar o app e o relay juntos; clientes antigos recebem
um erro de versão ao tentar entrar no DJ.
