# セキュリティと脆弱性の報告

Knowledge-Nest はローカルの信頼できる利用者が扱うプロジェクトを想定します。作業ディレクトリには原本、抽出済みの本文、索引、評価用の引用が入るため、OS のアクセス権で管理します。外部のモデルを使う構成では、処理に必要なテキストが設定した提供元へ送られます。

HTTP の既定の待ち受けはループバックです。外部からの接続を許す構成では、`serve.access` でトークン認証、許可する Host、許可する Origin を設定します。シークレットは環境変数に置き、必要に応じて TLS を終端する構成を用意します。 [HTTP の接続制限](docs/mcp.md#http-の接続制限)

## 脆弱性の報告

GitHub の非公開報告フォームから管理者へ連絡してください。非公開の報告でも認証情報や資料の本文は伏せ、再現に必要な情報を添えます。 [非公開で脆弱性を報告する](https://github.com/hachiware-labs/knowledge-nest/security/advisories/new) · [GitHub の案内](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately)

報告では Knowledge-Nest の版またはコミット、OS と Node.js の版、使用した種別、影響の説明を添えてください。現在は `main` の開発版を確認対象とします。
