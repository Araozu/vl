/** @param {string} text */
function escapeHtml(text) {
  return text.replace(/[&<>"']/g, (char) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[char]);
}

/** @param {string} name */
export function errorAnchor(name) {
  return name.toLowerCase().replace(/[^a-z0-9 _-]/g, '').trim().replace(/\s+/g, '-');
}

/**
 * @typedef {{ id: string, module: string, type_names?: string[], errors?: Array<{name: string}> }} Module
 * @typedef {{ name: string, type_params?: string[], params: Array<{name: string, type: string}>, returns: {type: string} }} FunctionSignature
 */

/** @param {Module[]} modules */
export function createTypeLinks(modules) {
  const links = new Map([
    ...['u8', 'u64', 'i64', 'f64', 'bool'].map((type) => [type, '/docs/basics#values-and-types']),
    ['void', '/docs/functions#functions-that-return-nothing'],
    ...['Numeric', 'Comparable'].map((type) => [type, '/docs/arrays#constrained-generics']),
  ]);
  for (const mod of modules) {
    for (const type of mod.type_names ?? []) {
      if (links.has(type)) throw new Error(`Duplicate stdlib type link: ${type}`);
      links.set(type, `/std/${mod.id}`);
    }
    for (const error of mod.errors ?? []) {
      const qualified = `${mod.module}.${error.name}`;
      links.set(qualified, `/std/${mod.id}#${errorAnchor(qualified)}`);
    }
  }
  return links;
}

/**
 * Link identifiers inside compound types, preserving all punctuation. Local
 * generic parameters shadow documented types; unknown types stay plain text.
 * @param {string} type
 * @param {string} module
 * @param {Map<string, string>} links
 * @param {string[]} generics
 */
export function renderType(type, module, links, generics = []) {
  return type.split(/([A-Za-z_][A-Za-z_0-9]*(?:\.[A-Za-z_][A-Za-z_0-9]*)*)/g).map((part, index) => {
    const href = index % 2 && !generics.includes(part)
      ? links.get(`${module}.${part}`) ?? links.get(part) : undefined;
    return href ? `<a href="${escapeHtml(href)}">${escapeHtml(part)}</a>` : escapeHtml(part);
  }).join('');
}

/** @param {string} module @param {FunctionSignature} fn */
export function signature(module, fn) {
  const params = fn.params.map((p) => `${p.name}: ${p.type}`).join(', ');
  const typeParams = fn.type_params?.length ? `[${fn.type_params.join(', ')}]` : '';
  return `${module}.${fn.name}${typeParams}(${params}) -> ${fn.returns.type}`;
}

/** @param {string} module @param {FunctionSignature} fn @param {Map<string, string>} links */
export function renderSignature(module, fn, links) {
  const generics = (fn.type_params ?? []).map((param) => param.split(/\s/)[0]);
  const render = (/** @type {string} */ type) => renderType(type, module, links, generics);
  const typeParams = fn.type_params?.length ? `[${fn.type_params.map(render).join(', ')}]` : '';
  const params = fn.params.map((p) => `${escapeHtml(p.name)}: ${render(p.type)}`).join(', ');
  return `${escapeHtml(`${module}.${fn.name}`)}${typeParams}(${params}) -&gt; ${render(fn.returns.type)}`;
}
