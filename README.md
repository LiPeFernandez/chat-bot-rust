# Chat Bot Rust

Agente com interface de chat. Um modelo compatível com a API OpenAI decide os passos; os comandos de shell e o código Python rodam dentro de um container Docker efêmero, com CPU, memória, usuário e tempo limitados. A saída (`stdout`, `stderr` e código de saída) volta para o modelo, que pode corrigir e tentar de novo.

O comando do modelo nunca é executado no host. O socket do Docker não entra no container.

## Pré-requisitos

- Rust 1.75 ou mais recente (`rustup`)
- Docker em execução (Docker Desktop no macOS, ou daemon em Linux)
- Um servidor de chat compatível com a API OpenAI e com tool calling

  Exemplos: Ollama, vLLM, xAI, DeepInfra, OpenAI.

A imagem padrão é `python:3.11-slim`. Ela tem `python` e `/bin/sh`. Não tem `bash`. O tool `execute_bash` usa o binário de `SANDBOX_SHELL` (padrão `/bin/sh`).

## Configuração

```bash
cp .env.example .env
```

| Variável | Padrão | Função |
| --- | --- | --- |
| `LLM_API_KEY` | `ollama` | Chave da API. Ollama ignora o valor; xAI, DeepInfra e OpenAI exigem uma chave real. |
| `LLM_BASE_URL` | `http://localhost:11434/v1` | Base OpenAI, sem barra no final. O cliente chama `{base}/chat/completions`. |
| `LLM_MODEL` | `qwen2.5-coder:7b` | Nome do modelo no servidor. |
| `LLM_TIMEOUT_SECS` | `120` | Timeout HTTP do pedido ao modelo. |
| `LLM_MAX_TOKENS` | `2048` | Limite de tokens da resposta. |
| `LLM_TEMPERATURE` | `0.2` | Temperatura. |
| `SANDBOX_IMAGE` | `python:3.11-slim` | Imagem do container. |
| `SANDBOX_NETWORK` | `none` | `none` (sem rede) ou `bridge`. Outros valores, inclusive `host`, são recusados. |
| `SANDBOX_MEMORY_MB` | `512` | Teto de RAM. Swap extra fica desligado (`MemorySwap = Memory`). |
| `SANDBOX_CPUS` | `1.0` | Quota de CPU via `NanoCpus` (1.0 = 1 CPU). |
| `SANDBOX_TIMEOUT_SECS` | `30` | Tempo máximo de cada comando. Ao estourar, o container recebe `SIGKILL` e é removido. |
| `SANDBOX_USER` | `1000:1000` | uid:gid. `root` e `0` são recusados. |
| `SANDBOX_PIDS_LIMIT` | `128` | Teto de processos. |
| `SANDBOX_SHELL` | `/bin/sh` | Binário do shell dentro da imagem. |
| `SANDBOX_PYTHON` | `python` | Binário Python dentro da imagem. |
| `AGENT_MAX_ITERATIONS` | `10` | Máximo de rodadas de ferramentas por mensagem do usuário. |
| `WEB_PORT` | `8787` | Porta da interface em `http://127.0.0.1:8787`. |
| `RUST_LOG` | `info` | Filtro do `tracing`. |

### Ollama

```bash
ollama pull qwen2.5-coder:7b
```

```env
LLM_API_KEY=ollama
LLM_BASE_URL=http://localhost:11434/v1
LLM_MODEL=qwen2.5-coder:7b
```

O modelo precisa conseguir emitir tool calls. Se ele só escrever texto, o agente também aceita um bloco explícito:

````
```tool
{"name":"execute_bash","arguments":{"command":"uname -a"}}
```
````

JSON solto no meio da frase não é executado.

### vLLM

```env
LLM_API_KEY=none
LLM_BASE_URL=http://localhost:8000/v1
LLM_MODEL=nome-do-modelo-servido
```

### xAI

```env
LLM_API_KEY=xai-...
LLM_BASE_URL=https://api.x.ai/v1
LLM_MODEL=grok-2-latest
```

### DeepInfra

```env
LLM_API_KEY=...
LLM_BASE_URL=https://api.deepinfra.com/v1/openai
LLM_MODEL=meta-llama/Meta-Llama-3.1-70B-Instruct
```

## Execução

Na raiz do projeto:

```bash
cargo check
cargo test
cargo run
```

`cargo run` sobe o chat em `http://127.0.0.1:8787` e tenta abrir o navegador. A página segue o uso de um chat: conversas na lateral, thread no centro e campo embaixo. As conversas ficam em `data/sessions.json`. Cada mensagem pode executar `execute_bash` ou `execute_python` na sandbox, e o resultado entra na thread. Dá para anexar um arquivo de texto e ditar no Chrome. `Nova conversa` abre outro fio e encerra o container atual.

O terminal continua disponível:

```bash
cargo run -- --cli
```

Na primeira execução o programa fala com o daemon e, se a imagem ainda não existir, faz o pull. O container em si só sobe no primeiro tool call.

```
você> crie um arquivo /tmp/oi.txt com a palavra ping e mostre o conteúdo
```

Comandos da sessão:

- `:help` — lista os comandos
- `:reset` — apaga o histórico e destrói o container
- `sair`, `exit`, `quit`, `:q` — encerra e remove a sandbox
- Ctrl+C — mesmo encerramento

## Arquitetura

```
você → main.rs → agent.rs → llm/ → API OpenAI-compatible
                  │
                  └─ tool_call → sandbox/ → container Docker
                                      │
                                      └─ stdout/stderr → role: tool → llm
```

1. `main.rs` lê a linha, trata Ctrl+C e delega o turno.
2. `agent.rs` manda o histórico e as ferramentas ao modelo.
3. Texto sem ferramenta é a resposta final.
4. `execute_bash` ou `execute_python` vão para a sandbox.
5. O resultado entra no histórico como mensagem `role: tool`.
6. O modelo recebe de novo e decide se corrige o comando ou responde.
7. Na rodada seguinte ao limite `AGENT_MAX_ITERATIONS`, o ciclo aborta.

## Isolamento

Cada container é criado com:

- `AutoRemove: true`
- usuário não-root (`SANDBOX_USER`, padrão `1000:1000`)
- memória rígida e swap limitado à mesma quantidade
- quota de CPU (`NanoCpus`; o daemon recusa `CpuPeriod` junto com `NanoCpus`)
- `PidsLimit`
- `CapDrop: ["ALL"]`
- `SecurityOpt: ["no-new-privileges:true"]`
- `Privileged: false`
- rede `none` ou `bridge` apenas
- sem volumes, sem portas publicadas e sem o socket do Docker
- `tmpfs` em `/tmp`
- processo principal `/bin/sleep infinity`, substituindo o entrypoint da imagem
- `tokio::time::timeout` em volta da exec

Se o comando passar do tempo (um `apt-get` parado em `y/n`, por exemplo), o stream é cancelado, o container recebe `SIGKILL` e a mensagem de timeout volta para o modelo. Comandos seguintes sobem um container novo.

A rede padrão é `none`. `bridge` libera saída para a internet e, dependendo do daemon, alcança a rede do host. Use `bridge` só quando o comando precisar baixar algo.

O usuário do container não é root. `apt-get install` falha por permissão. Isso é o comportamento esperado.

Payloads vazios, com byte nulo ou acima de 64 KiB são recusados antes da exec. `stdout` e `stderr` são limitados a 64 KiB cada para não estourar o contexto do modelo.

## Layout

```
Cargo.toml
.env.example
src/main.rs                 loop do terminal
src/config.rs               variáveis de ambiente
src/agent.rs                loop, tool calls e circuit breaker
src/utils.rs                erros e formatação do terminal
src/llm/mod.rs              cliente HTTP OpenAI-compatible
src/llm/types.rs            schema das ferramentas e parser textual
src/sandbox/mod.rs          API da sandbox
src/sandbox/docker.rs       bollard: pull, create, exec, kill
src/sandbox/security.rs     limites, usuário, rede e sanitização
```

## Solução de problemas

**Não foi possível falar com o daemon Docker.** Abra o Docker Desktop. No macOS, se o socket padrão não responder, o programa também tenta `~/.docker/run/docker.sock`. Dá para forçar com `DOCKER_HOST=unix:///Users/<você>/.docker/run/docker.sock`.

**O modelo descreve o comando em vez de chamar a ferramenta.** Confirme que o servidor implementa `tools` no endpoint de chat. Como reserva, o system prompt pede um bloco ` ```tool `.

**`python` ou `sh` não encontrado.** A imagem não traz esse binário. Ajuste `SANDBOX_IMAGE`, `SANDBOX_PYTHON` ou `SANDBOX_SHELL`.

**Timeout em todo comando.** O container pode não ter conseguido ficar de pé (usuário inexistente não é problema para uid numérico; entrypoint que exige root é). Veja o erro com `RUST_LOG=debug`.

**A resposta da API cita tool calling não suportado.** Troque o modelo por um que anuncie suporte a tools, ou use o bloco ` ```tool `.
