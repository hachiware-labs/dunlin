import { Dunlin } from 'dunlin';

/**
 * Orders the parser modules of a source plan. loader.auto is the fallback, so it is matched last
 * wherever it is written. The other parsers keep the written order.
 */
export function parserRoutes(entries) {
  const routes = entries.map(({ name, module, category, args }) => ({
    name, module, category, match: args.match, args,
    ocr: category === 'ocr' || (category === 'auto' && args.ocr === 'loader.ocr'),
  }));
  return [...routes.filter(({ category }) => category !== 'auto'), ...routes.filter(({ category }) => category === 'auto')];
}

export function defaultRoutes(ocr = false) {
  return parserRoutes([{ name: 'loader.auto', module: 'loader.auto', category: 'auto', args: { ocr: ocr ? 'loader.ocr' : 'none' } }]);
}

/**
 * Routes files to parser modules in Rust. Returns `{ route, kind }` for each file, or null when no
 * parser claims it.
 */
export function routeFiles(routes, files) {
  const byName = new Map(routes.map((route) => [route.name, route]));
  const plain = routes.map(({ name, category, match, ocr }) => ({ name, category, ...(match && { match }), ocr }));
  return Dunlin.routeFiles(plain, files).map(({ route, kind }) => route === null ? null : { route: byName.get(route), kind });
}
