# Python Script Workflow Examples

このディレクトリには、Open Workflow v1.0.0 の Script process を使用したPythonスクリプト実行の例が含まれています。

## 📁 ファイル一覧

### 1. `workflow-python-script-basic.yaml`
基本的なPythonスクリプト実行の例

**内容**:
- インラインPythonコードの実行
- パッケージの自動インストール (requests)
- 数値計算
- 文字列処理と正規表現

**実行方法**:
```bash
# ワークフローを実行
curl -X POST http://localhost:3000/api/workflows \
  -H "Content-Type: application/yaml" \
  --data-binary @workflow-python-script-basic.yaml

# または入力データをカスタマイズ
curl -X POST http://localhost:3000/api/workflows \
  -H "Content-Type: application/json" \
  -d '{
    "workflow": "@workflow-python-script-basic.yaml",
    "input": {
      "message": "Custom message",
      "count": 25
    }
  }'
```

### 2. `workflow-python-script-advanced.yaml`
高度なPythonスクリプト実行の例

**内容**:
- 外部スクリプトのHTTPSからの読み込み
- 複数パッケージの使用 (numpy, pandas)
- エラーハンドリング
- タスク間のデータフロー
- 条件分岐
- 仮想環境の再利用 (`script.use_static`)

**実行方法**:
```bash
# デフォルト設定で実行
curl -X POST http://localhost:3000/api/workflows \
  -H "Content-Type: application/yaml" \
  --data-binary @workflow-python-script-advanced.yaml

# GitHubリポジトリをカスタマイズ
curl -X POST http://localhost:3000/api/workflows \
  -H "Content-Type: application/json" \
  -d '{
    "workflow": "@workflow-python-script-advanced.yaml",
    "input": {
      "github_repo": "microsoft/vscode",
      "max_results": 10
    }
  }'
```

## 🔧 メタデータ設定

### 必須設定

#### `python.version`
使用するPythonバージョンを指定します。

```yaml
metadata:
  python.version: '3.12'  # または '3.11', '3.10', etc.
```

### オプション設定

#### `python.packages`
自動インストールするパッケージをカンマ区切りで指定します。

```yaml
metadata:
  python.version: '3.12'
  python.packages: 'requests,numpy,pandas'
```

#### `python.requirements_url`
requirements.txtのURLを指定します（`python.packages`と排他的）。

```yaml
metadata:
  python.version: '3.12'
  python.requirements_url: 'https://example.com/requirements.txt'
```

#### `script.use_static`
仮想環境を再利用して実行速度を向上させます。

```yaml
metadata:
  python.version: '3.12'
  python.packages: 'numpy,pandas'
  script.use_static: 'true'  # 仮想環境を再利用
```

**パフォーマンス比較**:
- `use_static: false`: 毎回新しいvenv作成（初回: ~2秒、以降: ~2秒）
- `use_static: true`: 初回のみvenv作成（初回: ~2秒、以降: ~0.1秒）

## 🔒 セキュリティ機能

### 1. Base64エンコードによるインジェクション防止

引数は自動的にBase64エンコードされ、Pythonコード内に安全に注入されます。

```yaml
script:
  code: |
    import json
    # message変数は自動的に安全に注入される
    result = {"message": message}
    print(json.dumps(result))
  arguments:
    message: ${ .input.message }  # どんな文字列でも安全
```

### 2. 危険な関数の検出

以下のパターンは実行前に拒否されます:

- `eval()`, `exec()`, `compile()`
- `__import__()`
- `os.system()`, `subprocess.*`
- Dunder属性アクセス（`__builtins__`, `__globals__`等、`__name__`, `__doc__`等は除く）

### 3. 外部スクリプトのHTTPS制限

```yaml
script:
  source:
    uri: https://trusted.com/script.py  # ✅ OK
    # uri: http://site.com/script.py    # ❌ 拒否
    # uri: file:///etc/passwd            # ❌ 拒否
```

## 📝 スクリプト記述のベストプラクティス

### 1. 出力はJSON形式で

```python
import json

result = {
    "status": "success",
    "data": {...}
}

print(json.dumps(result))
```

### 2. エラーハンドリング

```python
import json
import sys

try:
    # 処理
    result = {"status": "success"}
    print(json.dumps(result))
except Exception as e:
    error = {"status": "error", "message": str(e)}
    print(json.dumps(error), file=sys.stderr)
    sys.exit(1)  # 非ゼロで終了
```

### 3. タイムアウト設定

```yaml
- taskName:
    run:
      script:
        language: python
        code: |
          # 長時間実行される処理
    timeout: 60s  # タイムアウトを指定
```

### 4. 変数名の制約

Python識別子として有効な名前を使用してください:

```yaml
arguments:
  valid_name: "OK"      # ✅
  _private: "OK"         # ✅
  Name123: "OK"          # ✅
  # invalid-name: "NG"   # ❌ ハイフン不可
  # 1invalid: "NG"       # ❌ 数字開始不可
  # class: "NG"          # ❌ Python予約語不可
```

## 🎯 使用例

### Example 1: APIデータ取得と処理

```yaml
- fetchAndProcess:
    run:
      script:
        language: python
        code: |
          import json
          import requests

          url = api_url
          response = requests.get(url, timeout=10)
          data = response.json()

          # データ処理
          processed = {
              "count": len(data),
              "first_item": data[0] if data else None
          }

          print(json.dumps(processed))
        arguments:
          api_url: "https://api.example.com/data"
    metadata:
      python.version: '3.12'
      python.packages: 'requests'
```

### Example 2: データ分析

```yaml
- analyzeData:
    run:
      script:
        language: python
        code: |
          import json
          import numpy as np

          values = input_values
          result = {
              "mean": float(np.mean(values)),
              "std": float(np.std(values)),
              "max": float(np.max(values)),
              "min": float(np.min(values))
          }

          print(json.dumps(result))
        arguments:
          input_values: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    metadata:
      python.version: '3.12'
      python.packages: 'numpy'
```

### Example 3: ファイル処理（外部スクリプト）

```yaml
- processFile:
    run:
      script:
        language: python
        source:
          uri: https://raw.githubusercontent.com/example/scripts/main/file_processor.py
        arguments:
          file_path: ${ .input.file_path }
          encoding: "utf-8"
    metadata:
      python.version: '3.12'
      python.packages: 'chardet'
```

## 🐛 トラブルシューティング

### 問題: ModuleNotFoundError

**原因**: パッケージがインストールされていない

**解決策**:
```yaml
metadata:
  python.version: '3.12'
  python.packages: 'missing-package'  # ← 追加
```

### 問題: タイムアウトエラー

**原因**: 処理時間がデフォルトタイムアウトを超過

**解決策**:
```yaml
timeout: 300s  # タイムアウトを延長
```

### 問題: 変数が未定義

**原因**: arguments で定義していない

**解決策**:
```yaml
code: |
  # my_var を使用
  print(my_var)
arguments:
  my_var: ${ .input.value }  # ← 追加
```

## 📚 参考資料

- **実装ガイド**: `github/docs/workflow/script-process-security.md`
- **セキュリティテスト**: `github/app-wrapper/tests/script_security_tests.rs`
- **E2Eテスト**: `github/app-wrapper/tests/script_runner_e2e_test.rs`
- **Open Workflow Spec**: https://open-workflow-specification.org/schemas/1.0.0/workflow.json

## ⚠️ 注意事項

1. **セキュリティ**:
   - 外部スクリプトは信頼できるソースからのみ読み込んでください
   - ユーザー入力を直接コードに埋め込まず、必ず `arguments` 経由で渡してください

2. **パフォーマンス**:
   - 頻繁に実行されるタスクには `script.use_static: 'true'` を使用してください
   - 大量のパッケージインストールは初回実行時間が長くなります

3. **制限事項**:
   - Python識別子として無効な変数名は使用できません
   - ネストの深さは最大10レベルまでです
   - スクリプトサイズは1MB以下である必要があります（外部スクリプト）
