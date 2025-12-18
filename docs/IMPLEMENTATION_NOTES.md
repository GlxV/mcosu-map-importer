# Implementation Notes

## Módulos

- `app_state.rs`: modelos de configuração e estado (`BeatmapEntry`, `BeatmapMetadata`, `ImportStatus`), sanitização de nomes.
- `watcher.rs`: monitoramento de novos `.osz` com `notify` e checagem de estabilidade (tamanho + leitura de bytes).
- `osu_parser.rs`: parser simples para seções `[Metadata]` e `[Events]` de `.osu` (captura título, artista, mapper, versões/dificuldades, IDs, background).
- `osz_reader.rs`: abre o ZIP (`.osz`), agrega metadados das `.osu`, extrai imagem de background e gera thumbnail cacheado por hash.
- `importer.rs`: cria pasta sanitizada de destino, extrai arquivos do ZIP e marca duplicados se a pasta já existir.
- `cache.rs`: leitura/gravação de `config.json`, `cache.json`, pastas de cache de thumbnails e índice de duplicados (BeatmapSetID/hash). Armazena dados em diretório do usuário (`mcosu-importer` via `directories`).
- `ui/main.slint`: layout Slint com barra de controle, lista de cards e painel de logs; callbacks para ações.
- `main.rs`: orquestração, threads para watcher e processamento, ponte de dados entre backend e UI; logging em console + `logs/app.log`.
- Diretórios de dados: `%LOCALAPPDATA%/mcosu-importer` (ou equivalente em outros SOs) com `config.json`, `cache/` (thumbnails e cache.json) e `logs/app.log`.

## Fluxo de estados do card

1. `Detectado`: novo `.osz` apareceu.
2. `Aguardando`: aguardando arquivo estabilizar.
3. `Metadados` (ReadingMetadata): lendo `.osu` dentro do ZIP, pegando background.
4. `Importando`: extraindo para pasta `Songs`.
5. `Concluído` ou `Duplicado`: importação finalizada ou pasta já existia.
6. `Falhou`: erro em qualquer etapa (mostrado no card/log).

## Detecção de download concluído

- Ao receber evento de novo `.osz`, o worker checa estabilidade com `is_file_stable`:
  - Observa tamanho e mtime a cada `interval_ms` (default 700ms); precisa de `consecutive_checks` leituras iguais (default 3) e respeita `timeout_secs` (default 120).
  - Tenta abrir e ler bytes; falha se não conseguir.
  - Se não estabilizar até o timeout, marca como falha.

## Background e thumbnail

- Parser procura linha de background em `[Events]` com referência a `.jpg/.png`.
- `osz_reader` carrega esse arquivo do ZIP, gera thumbnail 256x256 (`image::thumbnail`).
- O hash `blake3` do `.osz` é usado como chave em `cache.json` e nome do arquivo em `cache/thumbnails/<hash>.png`.
- Se thumbnail já estiver em cache, não reprocessa.

## Duplicados
- Prioriza BeatmapSetID para detectar duplicados; fallback no hash do `.osz`.
- Índice salvo em `cache/cache.json` aponta BeatmapSetID/hash para a pasta de destino importada.
- Estado “Duplicado” mostra opções: Abrir destino, Reimportar (sobrescrever), Ignorar.
