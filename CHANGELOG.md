# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to Semantic Versioning when releases are tagged.

## [Unreleased]
### Added
- UI reorganizada: barra superior em duas linhas (configuracoes e acoes), cards com thumbnail fixa, badges suaves de status e botoes sem sobreposicao.
- Aba Links para abrir URLs de download no navegador (sem download interno), exibindo erro de validacao e lembrando o ultimo link.
- Preview de audio por beatmap (rodio) com cache local e controle unico play/pause por vez.
- Preview externo do beatmap usando viewer estatico vendorizado servido em `127.0.0.1`, abrindo nova janela do navegador (tenta modo app em Edge/Chrome) e registrando porta/pasta/URL no log.
- Auto-import sempre inicia desligado em cada abertura; auto-importa apenas itens detectados apos o toggle estar ligado. Botao global "Importar ja" dispara a fila pronta sem travar a UI.
- Botao de exclusao da fonte `.osz` apos concluir a importacao (usa Lixeira quando possivel e avisa se cair no delete definitivo), desabilitado quando o arquivo estiver fora da pasta de Downloads ou quando houver risco com a pasta `Songs`.
- Avisos/validacao quando `Songs` esta dentro (ou igual a) `Downloads`, bloqueando a exclusao da fonte para evitar apagar o destino.
- UI dos cards reorganizada: thumbnail fixa, infos em linhas separadas, botoes somente dentro do card e sem barra duplicada; textos longos com melhor corte/ajuste e lista com espacamento consistente.
- Botao global "Limpar concluidos" (UI only) e caixa "Mostrar concluidos" para arquivar itens finalizados/ignorados sem tocar nos arquivos.
- Toggle "Excluir fonte apos importar" com confirmacao (opcao "Nao perguntar novamente") usando Lixeira/fallback e bloqueios extras para configuracoes inseguras.
- Validacao reforcada de caminhos: impede escolher `Songs` dentro/igual a Downloads, mostra aviso persistente e desabilita auto-import/Importar ja/exclusao de fonte ate corrigir.
- Protecao contra cliques repetidos: flag/mutex para `Importar ja` e lock por item para evitar importacoes paralelas duplicadas.
- Linha de erro curta + botao "Detalhes" nos cards para extracao, escrita em destino, metadados ou exclusao da fonte.
- Release profile otimizado (lto thin, codegen-units 1, panic abort) e scripts de empacotamento (`scripts/package.ps1`, `scripts/package.sh`) gerando `dist/`.
- Diretório de dados via `directories`, com migração automática de `config.json`/`cache.json` da pasta atual para `%LOCALAPPDATA%/mcosu-importer` (ou equivalente).
- Logs em `logs/app.log`, painel com níveis INFO/WARN/ERROR e botão “Copiar logs”.
- Heurística de estabilidade configurável (`consecutive_checks`, `interval_ms`, `timeout_secs`) usando tamanho e mtime.
- Detecção de duplicados por BeatmapSetID ou hash, índice no cache e botões Reimportar/Ignorar/Abrir destino.
- Proteção contra Zip Slip e sanitização de caminhos ao extrair `.osz` (com testes).
- UI refinada: cards com destino, botões extras, logs com cores, placeholder de thumbnail.
- Pré-varredura da pasta monitorada na inicialização para enfileirar `.osz` existentes.
- Documentação atualizada (README, IMPLEMENTATION_NOTES) e licença MIT.

### Known Issues / Limitations
- Drag-and-drop depende do backend; use "Adicionar .osz" se falhar.
- Nao calcula estrelas/pp.
- Preview do beatmap depende do navegador padrao; modo app so funciona se Edge/Chrome estiverem disponiveis e o viewer roda em 127.0.0.1 usando cache/preview.
- Duplicados por BeatmapSetID/hash; nao compara conteudo arquivo a arquivo.
- Thumbnails so aparecem se o `.osz` trouxer referencia valida de background.

### Security / Privacy Notes
- Sem scraping, login, cookies ou chamadas autenticadas ao osu!; o app só processa arquivos locais.
- Não envia dados pela rede; a única ação externa opcional é abrir a página do beatmap quando há `BeatmapSetID`.
