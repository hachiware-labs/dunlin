// The JavaScript Dunlinfile reader that Dunlin.parseConfig replaced. Tests compare the two.
import { LineCounter, parseDocument } from 'yaml';

export function referenceParseConfig(text) {
  const counter = new LineCounter();
  const yaml = parseDocument(text, { lineCounter: counter });
  if (yaml.errors.length) {
    const error = yaml.errors[0];
    return { syntaxError: { line: error.linePos?.[0]?.line ?? null, message: 'Invalid YAML syntax or duplicate key.' } };
  }
  let config;
  try { config = yaml.toJS({ maxAliasCount: 0 }); }
  catch { return { syntaxError: { line: null, message: 'YAML aliases are unsupported.' } }; }
  const line = (path) => {
    for (let length = path.length; length > 0; length--) {
      const node = yaml.getIn(path.slice(0, length), true);
      if (node?.range) return counter.linePos(node.range[0]).line;
    }
    return null;
  };
  return { config, line };
}
