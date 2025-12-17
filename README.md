# McOsu Importer (Rust + Slint)

Aplicativo desktop leve para Windows (compatível com outros SOs) que monitora a pasta de downloads e importa beatmaps `.osz` do osu! para a pasta `Songs` do McOsu, exibindo metadados, thumbnail, preview de audio e um viewer externo do mapa.

## Requisitos
- Rust estavel (testado com 1.80+).
- Windows 10/11 (backends Slint/notify tambem funcionam em Linux/macOS).

## Build e execucao
- Dev: `cargo run`
- Release: `cargo build --release`  
  - Executavel: `target/release/mcosu-importer.exe` (Windows) ou `target/release/mcosu-importer`
  - Rodar release: `./target/release/mcosu-importer.exe` (PowerShell) ou `./target/release/mcosu-importer`
- Perfil release usa `lto = "thin"`, `codegen-units = 1`, `opt-level = "z"` e `panic = "abort"` para binario menor.
- Empacotar:
  - Windows: `pwsh scripts/package.ps1`
  - Linux/macOS: `sh scripts/package.sh`
  - Artefatos ficam em `dist/` junto com README, CHANGELOG, LICENSE e `assets/`.

## Uso
1) Abra o app.  
2) Barra superior (2 linhas):
   - Linha 1: pastas de Downloads e `Songs` do McOsu (campos somente leitura + botoes `Escolher`). Avisos de caminho inseguro aparecem logo abaixo.
   - Linha 2: acoes (`Importar ja` destacado, `Adicionar .osz`, `Limpar concluidos`) e toggles (`Auto-import`, `Excluir fonte automaticamente`, `Mostrar concluidos`).
3) Fluxo:
   - Ao detectar `.osz`, o app espera estabilidade do download (tamanho e mtime iguais por N leituras).
   - Le metadados dos `.osu`, detecta background e gera thumbnail.
   - Importa para `Songs` somente se o toggle estiver ligado quando o item entrou na fila; caso contrario use `Importar ja` ou os botoes do card (Importar/Reimportar).  
4) Botoes por card:
   - Importar / Reimportar / Ignorar.
   - Abrir arquivo (fonte) / Abrir destino / Abrir no navegador (usa BeatmapSetID).
   - Preview de audio (play/pause unico por vez; cache em `%LOCALAPPDATA%/mcosu-importer/cache/audio/`).
   - Preview visual do beatmap (abre o viewer estatico em uma janela nova do navegador via servidor local em `127.0.0.1`; tenta `--app=<URL>` em Edge/Chrome quando possivel, senao usa o navegador padrao).
   - Excluir fonte (`.osz`): apos concluir a importacao, apaga apenas o `.osz` original em Downloads (confirmacao + Lixeira quando possivel). Fica desabilitado se o arquivo nao estiver em Downloads ou se houver conflito com a pasta `Songs`. Tambem pode ser ligado globalmente pelo toggle `Excluir fonte apos importar` (primeira vez pede confirmacao com "Nao perguntar novamente").
   - Mensagens de erro ficam resumidas na linha `Erro:` com botao `Detalhes` para ver o texto completo (extracao do zip, escrita em Songs, leitura de metadados ou exclusao da fonte).
   - Layout dos cards: thumbnail fixa a esquerda, info em linhas separadas e botoes organizados em linhas dedicadas (preview + acoes), com caminhos encurtados para evitar overlap.
5) `Adicionar .osz` permite enfileirar manualmente.
6) Aba `Beatmaps`: pesquise por nome/artista/mapper e clique em `Buscar` para consultar Beatconnect e Chimu.moe. Os resultados listam titulo, artista/mapper, fonte e botao `Download`. O download ocorre direto para a pasta de Downloads configurada; ao concluir, o `.osz` e enfileirado automaticamente.

### Preview de audio e mapa
- Audio: so toca um preview por vez; clicar em outro item pausa o anterior. Usa o arquivo de audio do `.osz` ou do destino importado e reusa cache em `%LOCALAPPDATA%/mcosu-importer/cache/audio/`.
- Beatmap: ao clicar em `Preview beatmap`, o app monta um `.osz` temporario (a partir da fonte ou da pasta de destino se a fonte ja foi apagada), coloca em `%LOCALAPPDATA%/mcosu-importer/cache/preview/<hash>/` e sobe um servidor local para servir o viewer estatico vendorizado em `assets/viewer/`. Abre uma janela nova do navegador; em Windows tenta Edge/Chrome com `--app=<URL>` para ficar sem abas, e cai para o navegador padrao se nao achar.
- Logs trazem a porta, pasta de cache e URL usada para debug.

### Onde fica a pasta Songs do McOsu?
Steam > Biblioteca > clique direito em McOsu > Gerenciar > Procurar arquivos locais > abra a pasta `Songs`.

### Detecao de download estavel
- Parametros em `config.json`: `consecutive_checks`, `interval_ms`, `timeout_secs` (defaults: 3, 700ms, 120s).
- Considera estavel apos N leituras seguidas sem alteracao de tamanho ou timestamp; se ultrapassar o timeout, falha.

### Seguranca de caminhos
- Ao escolher as pastas, o app impede selecionar `Songs` dentro (ou igual) da pasta de Downloads e avisa, mantendo a configuracao anterior.
- Configuracoes perigosas (Downloads e Songs iguais/aninhadas) exibem aviso persistente no topo e bloqueiam `Importar ja`, auto-import e exclusao da fonte (manual/automatica) ate corrigir o caminho.

### Duplicados
- Preferencia por `BeatmapSetID`; fallback em hash do `.osz`.
- Indice armazenado em `cache/cache.json`.
- Estados de duplicado oferecem: Abrir destino, Reimportar (sobrescrever), Ignorar.

### Exclusao da fonte (.osz)
- Visivel depois de Concluido; apaga apenas o arquivo `.osz` original em Downloads (o destino `Songs` fica intacto).
- Usa Lixeira/Recycle Bin quando possivel; se falhar tenta `remove_file` e registra no log (nao marca a importacao como falha, mas mostra aviso/detalhes).
- So aparece habilitado se a fonte estiver dentro da pasta de Downloads configurada e se nao houver conflito com a pasta `Songs`.
- Opcao global `Excluir fonte apos importar` repete a exclusao automaticamente apos Concluido; a primeira vez mostra confirmacao com "Nao perguntar novamente".

### Cache, config e logs
- Diretorio de dados: `%LOCALAPPDATA%/mcosu-importer` (Windows) ou equivalente XDG/Library em outros SOs.
- `config.json`: caminhos de downloads, songs, auto-import e estabilidade.
- `cache/cache.json`: thumbnails, cache de audio e indice de duplicados (BeatmapSetID e hash).
- Thumbnails: `cache/thumbnails/`.
- Preview de audio: `cache/audio/<hash>/`; preview visual/extracao: `cache/preview/<hash>/` (servido pelo HTTP local ao abrir o viewer).
- Logs: `logs/app.log`. Botao "Copiar logs" copia o painel atual para a area de transferencia.
- Limpar: feche o app e apague a pasta `mcosu-importer` em dados do usuario. Na primeira execucao, o app migra `config.json`/`cache.json` se ainda estiverem na pasta atual.

## Testes
```bash
cargo test
```

## Troubleshooting (8 bullets)
- Card preso em "Aguardando": verifique se o download terminou; ajuste `consecutive_checks`/`interval_ms` no `config.json`.
- "Arquivo nao estabilizou": timeout de 120s expirou; confira permissoes da pasta de downloads.
- Sem thumbnail: o `.osz` nao possui referencia valida de background ou o formato nao e suportado.
- "Duplicado": o BeatmapSetID ou hash ja esta no cache; use "Reimportar" para sobrescrever ou "Abrir destino" para reutilizar.
- Pasta de destino vazia: confirme a pasta `Songs` do McOsu e permissoes de escrita.
- Logs nao aparecem: confira `logs/app.log` no diretorio de dados; use "Copiar logs".
- UI nao abre: tente rodar `cargo run -q` pelo terminal e veja erros; valide drivers/GTK/Qt conforme backend do Slint.
- Build falhou em release: limpe com `cargo clean` e garanta toolchain recente (1.80+).

## Observacoes de seguranca/privacidade
- Sem download direto, scraping, login ou cookies; apenas arquivos locais.
- A unica acao externa opcional e abrir a pagina do beatmap no navegador quando existe BeatmapSetID.

## Known Issues / Limitations
- Nao calcula estrelas/pp.
- Drag-and-drop depende do backend; use "Adicionar .osz" se nao funcionar.
- Detecao de duplicado via BeatmapSetID ou hash (nao compara conteudo arquivo a arquivo).
- Thumbnails so aparecem se o `.osz` trouxer referencia valida de background.
- Preview do beatmap depende do navegador padrao; modo app so funciona se Edge/Chrome estiverem disponiveis. O viewer roda em `127.0.0.1` e usa cache em `cache/preview/`.
