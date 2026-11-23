# Choose what to serve and whom to let in

[日本語](serve.md)

These settings are for using RAG over HTTP, or the screen of a Wiki or PARA, from other machines. If you use MCP over `stdio` only, you need none of this.

What `nest serve` offers, and whose connections it accepts, is set in the `serve` section of `KnowledgeNestfile.yaml`. `init` writes every default, so change only what you need. These are the defaults for RAG:

```yaml
serve:
  transport: http
  host: 127.0.0.1          # address to listen on; by default only this machine
  port: 4318
  endpoints:               # what to offer; anything not listed is not started
    - responses            # POST /v1/responses: answers with sources
    - search               # POST /v1/search: results and context
    - mcp                  # /mcp: MCP over HTTP for agents
  access:
    allowed_hosts:         # names accepted as the request's Host
      - localhost
      - 127.0.0.1
    allowed_origins: []    # origins whose pages may call from a browser
    auth:
      type: none           # none or token
      token_env: null      # with token, the name of the variable holding it
  limits:
    max_request_bytes: 1048576
    request_timeout_seconds: 120
    max_concurrent: 8
```

**Endpoints** depend on the project type.

| Type | `transport` | `endpoints` you can choose | Default port |
|---|---|---|---|
| `mcp` | `stdio` only | none (writing `host` through `limits` is `ARG_CONFLICT`) | none |
| `rag` | `http` | `responses`, `search`, `mcp` | 4318 |
| `wiki`, `para` | `http` | `web` | 4319 |

- To offer search without the answering API, use `endpoints: [search, mcp]`.
- An endpoint the type does not have is `ARG_RANGE`.

**Whom to let in** (`access`) is checked in this order.

- **Client.** With `allowed_clients`, only the listed address ranges and the machine running the server may connect; anyone else gets `403 CLIENT_FORBIDDEN`. Write an address and the number of leading bits a client shares with it, such as `192.168.11.0/24` for every `192.168.11.*` address, or a single address such as `192.168.11.7`. Left out, clients are not limited by address.
- **Host.** A request whose `Host` is not in `allowed_hosts` gets `403 HOST_FORBIDDEN`. Write a host name or IP address without a port. If people reach the server by an internal name such as `nest.example.internal`, add that name.
- **Origin.** A browser request whose `Origin` is neither the server itself nor in `allowed_origins` gets `403 ORIGIN_FORBIDDEN`. Write it with the scheme and no path, such as `https://app.example.com`. Left empty, pages on other sites cannot call the server, so a page you open cannot send requests to the server on your machine.
- **Token.** When `host` is not a loopback address such as `127.0.0.1`, `auth.type: token` is required, unless `allowed_clients` lists only the ranges of home and office networks (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, link-local addresses and `fc00::/7`); then the server also starts with `auth.type: none`. Keep the token out of the configuration file: `token_env` names the environment variable that holds it. Clients send `Authorization: Bearer <token>`. The Wiki and PARA pages also accept Basic authentication, with the user name `knowledge-nest` and the token as the password.

An example that opens the server to other machines:

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [nest.example.internal]
    allowed_origins: [https://app.example.com]
    auth: { type: token, token_env: NEST_TOKEN }
```

An example that opens the server to a home or office network only. Add this machine's address on that network to `allowed_hosts`; other devices open it by that address, such as `http://192.168.11.16:4319/`.

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [localhost, 127.0.0.1, 192.168.11.16]
    allowed_clients: [192.168.11.0/24]
```

This example has no token: every device in the range can read and write. On the Wiki and PARA pages, it can also take in a file of the server's machine by its path. If the network has devices you do not trust, add `auth: { type: token, token_env: NEST_TOKEN }`.

**Setting it with a command.** Instead of editing the file, `nest config` writes the access settings.

```sh
# For every project on this machine (kept in ~/.knowledge-nest/config.yaml)
nest config set serve --host 0.0.0.0 --allowed-hosts localhost,127.0.0.1,192.168.11.16 --allowed-clients 192.168.11.0/24

# For one project only (rewrites that project's KnowledgeNestfile.yaml)
nest config set serve --allowed-clients 192.168.11.0/24 --project ./my-wiki

nest config list                              # the settings of this machine
nest config list --project ./my-wiki          # the settings of the project
nest config unset serve                       # remove the settings of this machine
nest config unset serve --project ./my-wiki   # put the standard values back in the project
```

- It sets `host`, `port`, `allowed_hosts`, `allowed_clients` and `auth`. A port is written as `--port 8080`. A port in the settings of the machine goes to every project that still has the standard port of its type (4318 or 4319), so when several projects run at once, choose a port for each with `--project`. Authentication is `--auth none`, or `--auth token --token-env NEST_TOKEN`. Lists are separated by commas. An empty value (`--allowed-clients ""`) removes that setting.
- **The project wins.** Where the project file differs from the standard value that `init` wrote, the project's value is used; where it still has the standard value, the machine's value is used. `nest serve --host` and `--port` win over both.
- The token itself is never stored. `--token-env` takes the name of an environment variable; put the value in that variable.
- A `host` outside loopback is refused unless `--allowed-clients` with home or office ranges, or `--auth token`, comes with it.

```sh
export NEST_TOKEN=...      # use a long random value
nest validate --json       # reports conflicts before you start the server
nest serve
```

- `nest serve --host` and `--port` change the values for that run only. The checks above apply to the values after the change.
- `serve` settings are not part of the build ID or search configuration ID. Changing the port or access needs no rebuild or new evaluation.
- There is no encryption and no limit by client address range. To serve inside an organization, leave those to a reverse proxy in front.

`serve` decides only which connections come in. Where models are reached is `base_url` in `models` (a local Ollama by default). There is no setting that allows or limits the web addresses to take documents from.

[HTTP limits](mcp.md#http-の接続制限) · [Spec of the serve settings](knowledge-nest-spec.md#275-serve-の設定項目) · [Full configuration files](config.md) (Japanese)
