# 第三者のソフトウェアとデータ

ここでは Knowledge-Nest の配布物に含まれる通知の場所と、同梱・依存関係の調査に使う情報を案内します。Knowledge-Nest 本体のライセンス状況は `LICENSE` に記載しています。

## 同梱済みのライセンス・通知

| コンポーネント | 配布物内での扱い | ライセンス・通知 |
| --- | --- | --- |
| Lindera 6.2.0 | 日本語キーワード索引の形態素解析に使用 | [Lindera の MIT License](licenses/lindera-LICENSE.txt) |
| MeCab IPADIC 2.7.0-20070801 | Lindera の機能を通じて辞書データをネイティブバイナリに同梱 | [IPADIC の著作権表示と条件](licenses/ipadic-NOTICE.txt) |

## バイナリと実行時の依存

| 範囲 | 主なコンポーネントとメタデータ上のライセンス |
| --- | --- |
| Rust のネイティブアドオン | Tantivy、NAPI-RS、rusqlite、pulldown-cmark は MIT。Rayon、memmap2、idna は MIT または Apache-2.0。ICU4X（icu_normalizer、icu_segmenter、icu_collator、icu_properties とその辞書や照合のデータ）は Unicode-3.0 |
| 文書の読み取りと OCR（ネイティブアドオン） | pdf_oxide、ort、image は MIT または Apache-2.0。oar-ocr は Apache-2.0。ONNX Runtime と OCR のモデルは `knowledge-nest add oar-ocr` で取得し、それぞれの配布条件に従う |
| 埋め込みモデルの呼び出し（ネイティブアドオン） | ureq は MIT または Apache-2.0。rustls は Apache-2.0、ISC、MIT のいずれか。ring は Apache-2.0 と ISC。rustls-webpki は ISC。webpki-roots（Mozilla のルート証明書）は CDLA-Permissive-2.0 |
| 近似ベクトル索引の補助バイナリ | hannoy、heed は MIT。その他の依存は `tools/hannoy/Cargo.lock` で版を特定 |
| npm の実行時に解決する依存 | なし。ネイティブアドオンと入口だけで動く。旧実装と比べるテストが使う MCP TypeScript SDK、cheerio、markdown-it、yaml、zod、npm の docling.rs は開発用の依存で、パッケージには含めない |
| 文書処理（ネイティブアドオン）と追加セットアップ | Office 文書の変換に使う docling.rs の crate（`docling`、`docling-core`）は MIT。HTML の解析に使う scraper と html5ever は ISC と MIT または Apache-2.0。追加で取得するモデルや実行環境はそれぞれの配布条件を参照 |

対象プラットフォームごとの依存全体は、`Cargo.lock`、`tools/hannoy/Cargo.lock`、`package-lock.json` から確認します。配布前の棚卸しでは `cargo metadata --locked --features node`、hannoy 側の `cargo metadata --locked --manifest-path tools/hannoy/Cargo.toml`、`npm ls --all --omit=dev` と各依存の通知本文を照合します。この表は主な依存への入口です。
