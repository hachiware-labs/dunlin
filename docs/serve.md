# 提供する入口と、接続を許す相手を決める

[English](serve.en.md)

RAG の HTTP と、Wiki・PARA の画面を、ほかのマシンからも使わせるときの設定です。MCP（`stdio`）だけで使うなら、この設定は要りません。

`nest serve` で何を出し、誰からの接続を受けるかは、`KnowledgeNestfile.yaml` の `serve` に書きます。`init` が既定値をすべて書き出すので、変えたい所だけを直します。次は RAG の既定値です。

```yaml
serve:
  transport: http
  host: 127.0.0.1          # 待ち受けるアドレス。既定では自分のマシンからだけ
  port: 4318
  endpoints:               # 出す入口。書かなかった入口は立ち上がらない
    - responses            # POST /v1/responses：出典付きの答え
    - search               # POST /v1/search：検索結果と文脈
    - mcp                  # /mcp：エージェント向けの MCP（HTTP）
  access:
    allowed_hosts:         # 要求の宛先名（Host）として受け付けるもの
      - localhost
      - 127.0.0.1
    allowed_origins: []    # 別のページのブラウザから呼んでよい送信元
    auth:
      type: none           # none か token
      token_env: null      # token のとき、合言葉を入れた環境変数の名前
  limits:
    max_request_bytes: 1048576
    request_timeout_seconds: 120
    max_concurrent: 8
```

**入口（`endpoints`）** は、種別ごとに選べるものが決まっています。

| 種別 | `transport` | 選べる `endpoints` | 既定のポート |
|---|---|---|---|
| `mcp` | `stdio` だけ | なし（`host` から `limits` までを書くと `ARG_CONFLICT`） | なし |
| `rag` | `http` | `responses`、`search`、`mcp` | 4318 |
| `wiki`、`para` | `http` | `web` | 4319 |

- 回答 API を出さず、検索だけを出すときは `endpoints: [search, mcp]` にします。
- 種別にない入口を書くと `ARG_RANGE` になります。

**接続を許す相手（`access`）** は、次の順に確かめられます。

- **接続元。** `allowed_clients` を書くと、そこに挙げたアドレスの範囲と、サーバーを動かしているマシン自身からの接続だけを受け付けます。ほかは `403 CLIENT_FORBIDDEN` です。`192.168.11.0/24`（`192.168.11.*` のこと）のように、アドレスと、先頭から一致させるビット数で書きます。1台だけなら `192.168.11.7` のようにアドレスだけを書きます。書かなければ、接続元では絞りません。
- **宛先名。** 要求の `Host` が `allowed_hosts` にないと `403 HOST_FORBIDDEN` です。ホスト名か IP アドレスを、ポートを付けずに書きます。社内の名前（`nest.example.internal` など）で呼ばせるときは、その名前を足します。
- **送信元。** ブラウザが付ける `Origin` が自分自身でも `allowed_origins` にもないと `403 ORIGIN_FORBIDDEN` です。`https://app.example.com` のように、スキームを含め、パスを付けずに書きます。空のままなら、ほかのサイトのページからは呼べません。開いたページが手元のサーバーに勝手に要求を送るのを防ぐためです。
- **合言葉。** `host` を `127.0.0.1` などのループバック以外にするときは、`auth.type: token` が必須です。ただし、`allowed_clients` が家や職場のネットワークの範囲（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、リンクローカル、`fc00::/7`）だけなら、`auth.type: none` のままでも起動できます。合言葉は設定ファイルに書かず、`token_env` に環境変数の名前だけを書きます。クライアントは `Authorization: Bearer <token>` を送ります。Wiki・PARA の画面は Basic 認証も受け付け、ユーザー名は `knowledge-nest`、パスワードは同じ合言葉です。

ほかのマシンに開くときの例です。

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [nest.example.internal]
    allowed_origins: [https://app.example.com]
    auth: { type: token, token_env: NEST_TOKEN }
```

家や職場の LAN の中だけに開くときの例です。`allowed_hosts` には、このマシンの LAN でのアドレスを足します。ほかの端末は `http://192.168.11.16:4319/` のように、そのアドレスで開きます。

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [localhost, 127.0.0.1, 192.168.11.16]
    allowed_clients: [192.168.11.0/24]
```

この例は合言葉なしです。範囲の中の端末なら、だれでも読み書きできます。Wiki・PARA の画面では、サーバーのマシンにあるファイルをパスで取り込むこともできます。同じネットワークに信用できない端末があるなら、`auth: { type: token, token_env: NEST_TOKEN }` を足してください。

**コマンドで設定する。** 接続の設定は、ファイルを編集する代わりに `nest config` でも書けます。

```sh
# この PC のすべてのプロジェクトに効かせる（~/.knowledge-nest/config.yaml に保存）
nest config set serve --host 0.0.0.0 --allowed-hosts localhost,127.0.0.1,192.168.11.16 --allowed-clients 192.168.11.0/24

# 1つのプロジェクトだけに効かせる（そのプロジェクトの KnowledgeNestfile.yaml を書き換える）
nest config set serve --allowed-clients 192.168.11.0/24 --project ./my-wiki

nest config list                              # この PC の設定を見る
nest config list --project ./my-wiki          # プロジェクトの設定を見る
nest config unset serve                       # この PC の設定を消す
nest config unset serve --project ./my-wiki   # プロジェクトの設定を標準の値に戻す
```

- 設定できるのは `host`、`port`、`allowed_hosts`、`allowed_clients`、`auth` です。ポートは `--port 8080` のように書きます。この PC の設定にポートを書くと、種別ごとの標準のポート（4318、4319）のままのプロジェクトがすべてそのポートで待ち受けるので、同時に動かすプロジェクトがあるなら `--project` でプロジェクトごとに決めてください。認証は `--auth none`、または `--auth token --token-env NEST_TOKEN` と書きます。一覧はコンマで区切ります。空の値（`--allowed-clients ""`）を渡すと、その項目を消します。
- **プロジェクトが優先です。** プロジェクトのファイルで標準の値（`init` が書いた値）から変えた項目は、プロジェクトの値を使います。標準の値のままの項目に、この PC の設定を使います。`nest serve --host` と `--port` は、どちらよりも優先します。
- 合言葉そのものは保存しません。`--token-env` には環境変数の名前を渡し、値は環境変数に入れます。
- ループバックの外の `host` は、家や職場の範囲の `--allowed-clients` か `--auth token` を一緒に渡さないと、保存を断ります。

```sh
export NEST_TOKEN=...      # 長い乱数にする
nest validate --json       # 矛盾があれば、起動する前にここで分かる
nest serve
```

- `nest serve --host` と `--port` は、その起動に限って値を変えます。上の検査は上書き後の値にもかかります。
- `serve` の値はビルド ID や検索設定 ID に入りません。ポートや許可を変えても、作り直しや評価のし直しは要りません。
- 通信の暗号化と、接続元のアドレスの範囲による制限はありません。社内に公開するときは、前に置く中継サーバー（リバースプロキシ）に任せます。

`serve` が決めるのは、受ける側の接続だけです。モデルへの接続先は `models` の `base_url`（既定はローカルの Ollama）で決まります。取り込む Web のアドレスを許可したり制限したりする設定はありません。

[HTTP の接続制限](mcp.md#http-の接続制限) · [serve の設定項目の仕様](knowledge-nest-spec.md#275-serve-の設定項目) · [設定ファイルの全文](config.md)
