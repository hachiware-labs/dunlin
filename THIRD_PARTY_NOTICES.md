# 第三者のソフトウェアとデータ

ここでは Dunlin の配布物に含まれる通知の場所と、同梱・依存関係の調査に使う情報を案内します。Dunlin 本体のライセンス状況は `LICENSE` に記載しています。

## 同梱済みのライセンス・通知

| コンポーネント | 配布物内での扱い | ライセンス・通知 |
| --- | --- | --- |
| Lindera 6.2.0 | 日本語キーワード索引の形態素解析に使用 | [Lindera の MIT License](licenses/lindera-LICENSE.txt) |
| MeCab IPADIC 2.7.0-20070801 | Lindera の機能を通じて辞書データをネイティブバイナリに同梱 | [IPADIC の著作権表示と条件](licenses/ipadic-NOTICE.txt) |

## バイナリと実行時の依存

| 範囲 | 主なコンポーネントとメタデータ上のライセンス |
| --- | --- |
| Rust のネイティブアドオン | Tantivy、NAPI-RS、rusqlite、pulldown-cmark は MIT。Rayon と memmap2 は MIT または Apache-2.0 |
| 近似ベクトル索引の補助バイナリ | hannoy、heed は MIT。その他の依存は `tools/hannoy/Cargo.lock` で版を特定 |
| npm の実行時に解決する依存 | MCP TypeScript SDK の `@modelcontextprotocol/server` は Apache-2.0。cheerio、markdown-it、zod は MIT。yaml は ISC |
| 文書処理の任意依存と追加セットアップ | `pdfjs-dist` は Apache-2.0、`docling.rs` と `@napi-rs/canvas` は MIT。追加で取得するモデルや実行環境はそれぞれの配布条件を参照 |

対象プラットフォームごとの依存全体は、`Cargo.lock`、`tools/hannoy/Cargo.lock`、`package-lock.json` から確認します。配布前の棚卸しでは `cargo metadata --locked --features node`、hannoy 側の `cargo metadata --locked --manifest-path tools/hannoy/Cargo.toml`、`npm ls --all --omit=dev` と各依存の通知本文を照合します。この表は主な依存への入口です。
