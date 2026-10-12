# DJ compartilhado

O DJ é uma sessão do FastDiscord por **servidor + canal de voz**, sem bot.
Todos os participantes com a mesma configuração de DJ recebem a mesma
playlist, posição e controles. A música é reproduzida localmente: não sai
pelo microfone e não é ouvida no Discord oficial ou em versões antigas do app.

## Usar no app

Não há nada para configurar: nenhum servidor, endereço ou chave.

1. Entre na sala de voz e clique no botão de música/DJ.
2. Cole um link HTTPS do YouTube para adicionar diretamente, ou pesquise pelo
   nome para ver até dez resultados com título e duração. Escolha **Adicionar**
   no resultado desejado. A duração é conferida antes de compartilhar o vídeo.
   Repita para montar a playlist.
3. O accordion aparece acima de **Voz conectada** para todos os participantes.
   Tem **voltar**, **play/pause**, **próxima**, **parar** e **playlist**.
4. O slider **Volume** afeta apenas você; o deafen também silencia o DJ.

Todos podem adicionar e controlar; o líder ordena os comandos recebidos.
Voltar reinicia a música se ela já passou de três segundos, ou volta à anterior.
Parar encerra a reprodução para todos e mantém a playlist para retomar com play.
Sair da voz encerra apenas o player local; a sessão continua para quem ficou.
Entrar depois ou reconectar recupera a música e a posição atual. A sessão é
apagada quando o último participante do DJ sai da sala.

## Como funciona (peer to peer)

Não existe servidor do DJ. Um dos apps na sala, o **líder**, hospeda a sessão em
memória (`src/backend/dj_server.rs`), e os outros se conectam a ele com
[iroh](https://docs.rs/iroh): QUIC discado por chave pública, com hole punching
e, quando a conexão direta falha (CGNAT, firewall), passando pelos relays
públicos e gratuitos da n0. Só comandos e metadados trafegam; o áudio cada app
baixa por conta própria. O próprio app do líder fala com a sessão por um canal
em memória, sem rede.

- **Identidade:** a chave do endpoint iroh de cada app é derivada do ID do
  Discord (`secret()` em `src/backend/dj.rs`). Quem conhece o ID de alguém da
  sala calcula o endereço dele, então ninguém precisa trocar endereços.
- **Autorização:** o líder só aceita conexões cuja chave corresponde a um
  membro atual do canal de voz (pelos voice states do gateway).
- **Eleição:** ao entrar, o app tenta todos os outros membros da sala em
  paralelo e segue quem já estiver liderando. Se ninguém lidera, quem está
  sozinho assume. Se dois começarem ao mesmo tempo, a cada 10 s o líder procura
  rivais e cede para quem tem música na playlist ou, empatando, o menor ID.
- **Troca de líder:** cada estado inclui a lista de apps conectados. Se o líder
  sai, o próximo menor ID dessa lista assume com a última playlist e posição,
  convertidas para o próprio relógio. Os demais esperam por ele (ou assumem
  após três tentativas sem resposta). Durante a troca o áudio para alguns
  segundos e é baixado de novo.

Modelo de confiança: a derivação é pública. Quem tiver o app e souber o ID do
Discord de um membro da sala pode se passar por ele no DJ, e isso permite
controlar a música e escolher vídeos do YouTube (IDs validados). É o mesmo
nível da antiga chave compartilhada do relay, que ia dentro do binário. O mesmo
usuário em dois dispositivos na mesma sala gera conflito de endpoint.

Os relays públicos da n0 têm limite de taxa e nenhuma garantia de
disponibilidade ([docs](https://docs.iroh.computer/relays/rate-limiting)). O
tráfego do DJ é pequeno, mas se eles ficarem fora do ar, só funciona quem
consegue conexão direta.

## Sincronização e limites

Cada app baixa a música inteira em uma pasta temporária local e confirma que
consegue decodificar o áudio. O líder aguarda as confirmações dos participantes
e programa o início três segundos adiante; o relógio não avança durante o download.
Cada app estima o relógio do líder pelo tempo de ida e volta e acompanha a posição.
Quem entra depois também baixa completamente e então busca a posição atual no
arquivo local. Falha de um participante não impede os demais de tocar.

Os temporários são removidos no fim, ao parar, trocar a música, sair da sala ou
encerrar normalmente o app. Nenhum áudio passa pela rede do DJ. Encerramento forçado
pode deixar arquivos na pasta temporária até a limpeza do sistema. Para evitar
ocupar todo o disco, o download tem limite de 512 MiB e cinco minutos.

Isso reduz diferenças de posição, mas não garante saída exatamente simultânea:
rede, download e buffers do dispositivo de áudio variam. Enquanto o áudio
carrega, o app mostra **Baixando: X/Y prontos**. Um vídeo indisponível
ou bloqueado para um participante produz erro local, sem parar os demais.
A sessão não depende de quem adicionou a música: se o líder sai, outro assume.

Por enquanto, são aceitos vídeos do YouTube com duração conhecida de até 24h;
lives não são aceitas. Todos os participantes precisam de `yt-dlp` e `ffmpeg`
(mais Deno para suporte completo ao YouTube). O pacote Windows já inclui os
auxiliares. Não são transmitidos cookies ou credenciais do YouTube.

O protocolo v3 limita mensagens a 1 MiB e a playlist a aproximadamente 512 KiB
de metadados, com erro explícito se ultrapassar esse tamanho. O líder limita
controles a 20 por segundo por conexão e remove clientes sem heartbeat por
15 segundos. Filas e sessões ficam apenas na memória.

Referências: [yt-dlp](https://github.com/yt-dlp/yt-dlp) e
[tokio-tungstenite](https://docs.rs/tokio-tungstenite/0.24.0/tokio_tungstenite/)
(o mesmo protocolo de mensagens roda sobre o stream QUIC) e
[iroh](https://docs.rs/iroh).

O protocolo v3 (P2P) não conversa com o relay v2: todos na sala precisam da
mesma versão do app. Versões antigas não encontram o DJ das novas.
