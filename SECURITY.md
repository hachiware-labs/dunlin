# セキュリティと脆弱性の報告

Dunlin はローカルの信頼できる利用者が扱うプロジェクトを想定します。作業ディレクトリには原本、抽出済みの本文、索引、評価用の引用が入るため、OS のアクセス権で管理します。外部のモデルを使う構成では、処理に必要なテキストが設定した提供元へ送られます。

HTTP の既定の待ち受けはループバックです。外部からの接続を許す構成では、`serve.access` でトークン認証、許可する Host、許可する Origin を設定します。シークレットは環境変数に置き、必要に応じて TLS を終端する構成を用意します。 [HTTP の接続制限](docs/mcp.md#http-の接続制限)

## 脆弱性の報告

GitHub の非公開報告機能は現時点で無効です。リポジトリの Issues で管理者に**非公開の連絡先だけ**を尋ね、案内された経路で詳細を共有してください。連絡先を尋ねる投稿は公開されます。非公開の報告でも認証情報や資料の本文は伏せ、再現に必要な情報を添えます。 [Dunlin の Issues](https://github.com/hachiware-labs/dunlin/issues/new) · [GitHub の非公開報告に関する案内](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately)

報告では Dunlin の版またはコミット、OS と Node.js の版、使用した種別、影響の説明を添えてください。現在は `main` の開発版を確認対象とします。
