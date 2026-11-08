import { extname } from 'node:path';

const extensions = { '.js': 'javascript', '.mjs': 'javascript', '.cjs': 'javascript', '.jsx': 'javascript',
  '.ts': 'typescript', '.tsx': 'tsx', '.py': 'python', '.rs': 'rust' };
const declarations = {
  javascript: ['function_declaration', 'class_declaration', 'method_definition'],
  typescript: ['function_declaration', 'class_declaration', 'method_definition', 'interface_declaration', 'type_alias_declaration'],
  python: ['function_definition', 'class_definition'],
  rust: ['function_item', 'struct_item', 'enum_item', 'trait_item'],
};
let library;

export function codeLanguage(name) {
  return extensions[extname(name).toLowerCase()];
}

async function load() {
  library ??= Promise.all([import('@ast-grep/napi'), import('@ast-grep/lang-python'), import('@ast-grep/lang-rust')])
    .then(([api, python, rust]) => {
      api.registerDynamicLanguage({ python: python.default, rust: rust.default });
      return api;
    }).catch((error) => { throw new Error('Could not load ast-grep and its Python/Rust grammars. Run npm ci in tools/rag.', { cause: error }); });
  return library;
}

export async function check() {
  for (const [name, text] of [['setup.ts', 'function dunlin() {}'], ['setup.py', 'def dunlin(): pass'], ['setup.rs', 'fn dunlin() {}']]) {
    if (!(await parseCode(Buffer.from(text), name)).locations.some(({ symbol }) => symbol === 'dunlin')) {
      throw new Error(`ast-grep failed its ${name} check`);
    }
  }
  return { languages: [...new Set(Object.values(extensions))] };
}

export async function parseCode(bytes, name) {
  const content = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  if (!content.trim()) throw new Error('Document has no text');
  const { parse } = await load();
  const language = codeLanguage(name);
  const kinds = declarations[language === 'tsx' ? 'typescript' : language];
  const nodes = parse(language, content).root().findAll({ rule: { any: kinds.map((kind) => ({ kind })) } });
  const locations = nodes.flatMap((node) => {
    const symbol = node.field('name')?.text();
    if (!symbol) return [];
    const { start, end } = node.range();
    return [{ symbol, range: { startLine: start.line + 1, endLine: end.line + (end.column ? 1 : 0) } }];
  });
  return { content, format: 'text', locations };
}
