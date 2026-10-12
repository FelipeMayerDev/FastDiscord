# Interface: referências do Discord e implementação

Pesquisa atualizada em 10 de outubro de 2026. A interface é nativa em egui;
os desenhos são vetoriais próprios, sem depender de fontes de emoji ou de
arquivos de animação proprietários do Discord.

## Referências

- [Discord Design Systems, agosto de 2026](https://discord.com/blog/improving-mobile-with-squircles-styles-and-spacing):
  pessoas e DMs usam círculos; servidores e apps usam formas com cantos
  arredondados. O artigo também descreve a redução de controles na barra
  de mensagens para deixar mais espaço para escrever.
- [Justin Lemmon, animações de ícones para Discord](https://www.justinlemmon.com/projects/test-icon-animations):
  animações durante interações, com movimento simples, rápido e coerente
  entre os ícones. O trabalho original usa vetores exportados em JSON.
- [Discord, redução de movimento](https://support.discord.com/hc/en-us/articles/360040613412-Reduced-Motion-Setting):
  opção de acessibilidade para reduzir animações da interface.

As durações usadas no FastDiscord são escolhas da implementação; as fontes
acima não publicam valores exatos para cada controle.

## Aplicação no cliente

Avatares continuam circulares, com status visível nas conversas individuais.
Servidores mantêm cantos arredondados e indicadores laterais de seleção e
mensagens não lidas. Os controles vetoriais usam cores do tema, contornos
consistentes e tooltips em português.

Hover, seleção e pressão têm transições curtas, sem animação permanente em
controles parados. A opção **Reduzir movimento da interface e GIFs**, em
Configurações → Aparência, torna essas transições imediatas e mantém os GIFs
no primeiro quadro. Transmissões de vídeo continuam sendo conteúdo reproduzido.

**Atividades** está fora da interface enquanto não houver implementação.
As ações de voz disponíveis são compartilhamento de tela, câmera, soundboard e
DJ; não deve haver espaço reservado para ações sem implementação. Compartilhar
tela abre um modal para escolher a fonte, a resolução e a taxa de quadros
([SCREENSHARE.md](SCREENSHARE.md#picker)).

## Já implementado

- Noto Sans e família em negrito; cores e espaçamento compartilhados.
- Configurações em modal centralizado, com fundo escurecido, navegação por
  seção, conteúdo rolável e abas no topo em janelas estreitas. Fecha por
  botão, Escape ou clique fora; atalhos de canais ficam bloqueados.
- Aba Perfil: avatar (recorte central, PNG de até 512 px), nome exibido,
  apelido no servidor selecionado, pronomes e "Sobre mim", com prévia do
  cartão de perfil.
- Separadores de datas, agrupamento de mensagens, respostas, indicador de
  novas mensagens, digitação, caixa multilinha e links clicáveis.
- Status próprio (Online, Ausente, Não perturbe, Invisível) e status
  personalizado, pelo avatar no painel do usuário ou pela bandeja.
- Mute/surdo na lista da sala de voz com os ícones do Discord: vermelhos
  quando o próprio membro ativa, em selo vermelho quando é o servidor.
- Avatares e status, convites com cartão, GIFs e visualizador de imagens.
- Lista de membros à direita, alternada pelo botão de pessoas do cabeçalho:
  grupos por cargo exibido separadamente e online/offline, vinda da lista
  preguiçosa do gateway (op 37); nas DMs, os participantes. Clicar numa
  resposta rola até a mensagem original e a destaca.
- Amigos e solicitações, busca no histórico em cache e arquivos.
- Seletores de emoji (Unicode e personalizados dos servidores), de GIF
  (endpoints de GIF do Discord, Tenor) e soundboard como popovers ancorados
  no botão, no padrão `egui::Popup`; fecham com clique fora ou Escape. O
  seletor de emoji só lista emojis Unicode de um glifo, que a fonte desenha.
- Quem toca um som do soundboard fica com o anel verde enquanto o som toca.
- Tópicos: os seguidos aparecem sob o canal pai; botão **Tópicos** no
  cabeçalho (ativos e arquivados públicos), "Criar tópico" no menu de
  contexto da mensagem e resumo "N mensagens".
- Fóruns no layout do Discord: barra "Buscar ou criar postagem..." (filtra
  pelo título; **Nova postagem** abre a criação com o texto digitado como
  título), diretrizes da postagem (o tópico do canal) recolhíveis e
  lembradas por canal, "Ordenar e ver" (atividade recente ou criação),
  tags como filtros (as que não cabem ficam em **Todos**) e cartões com
  tags, título, selo NOVO, "Autor: prévia", primeira reação, mensagens,
  idade e miniatura da primeira imagem. As arquivadas vêm de
  `threads/search` paginado na rolagem; as ativas, do gateway, com a
  primeira mensagem via `post-data`. NOVO aproxima a regra do Discord (sem
  read states): nunca aberta nem seguida e criada nos últimos 3 dias.
- Canal de voz ativo verde, tempo conectado, soundboard e DJ.
- Clicar nas barras de sinal de Voz conectada abre a qualidade da conexão:
  gráfico de ping dos últimos 5 minutos, ping atual/médio, perda de pacotes
  recebidos, servidor e codec; a cor das barras segue o ping e a perda.
- Bandeja no Linux e Windows, com restaurar janela, sair e continuar chamadas
  em segundo plano ao fechar a janela.

## Pendências de paridade

Pastas e ordem de servidores dependem do settings protobuf do Discord.
Reações, pins, escolha de tags ao criar postagem, galeria de fórum e
renderização completa de emojis personalizados
continuam no roteiro de [PORT.md](PORT.md). Os nomes e capacidades dos
controles devem acompanhar recursos realmente implementados.

## DJ compartilhado

O botão DJ abre a playlist em um modal no padrão das configurações. Uma sessão
ativa mostra um accordion acima de Voz conectada, com voltar, play/pause,
próxima, parar e playlist. Todos na mesma sala com o mesmo relay recebem os
controles e a fila; o volume é local. O DJ não cria um usuário na sala nem envia
áudio pelo microfone. Configuração e operação: [DJ.md](DJ.md).
