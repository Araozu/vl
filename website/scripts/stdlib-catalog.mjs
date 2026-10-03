// Compiler-owned structure with optional editorial annotations. Iterating the
// compiler catalog makes new modules/functions visible without YAML edits.
export function mergeCatalog(api, editorial) {
  const modules = new Map(api.map((module) => [module.module, module]));
  const annotations = new Map(editorial.map((module) => [module.module, module]));
  for (const entry of editorial) {
    const module = modules.get(entry.module);
    if (!module) throw new Error(`Stale stdlib prose: module ${entry.module}`);
    for (const kind of ['functions', 'errors']) {
      const names = new Set(module[kind].map((item) => item.name));
      for (const item of entry[kind] ?? []) {
        if (!names.has(item.name)) {
          throw new Error(`Stale stdlib prose: ${entry.module}.${item.name}`);
        }
      }
    }
  }
  const result = api.map((module, index) => {
    const prose = annotations.get(module.module);
    const leaf = module.module.split('.').at(-1);
    return {
      id: prose?.id ?? leaf,
      order: prose?.order ?? editorial.length + index + 1,
      module: module.module,
      type_names: prose?.type_names ?? [],
      source: prose?.source,
      summary: prose?.summary ?? `Functions exported by ${module.module}.`,
      title: prose?.title ?? `${module.module} module`,
      description: prose?.description ?? `Standard library reference for ${module.module}.`,
      intro: prose?.intro ?? `Import \`${module.module}\` to use its exported functions.`,
      import_example: prose?.import_example ?? `use ${module.module};\n`,
      outro: prose?.outro,
      functions: module.functions.map((fn) => {
        const note = prose?.functions?.find((item) => item.name === fn.name);
        for (const param of note?.params ?? []) {
          if (!fn.params.some((item) => item.name === param.name)) {
            throw new Error(`Stale parameter prose: ${module.module}.${fn.name}.${param.name}`);
          }
        }
        return {
          name: fn.name,
          type_params: fn.type_params,
          params: fn.params.map((param) => ({
            ...param,
            detail: note?.params?.find((item) => item.name === param.name)?.detail,
          })),
          returns: { ...fn.returns, detail: note?.returns?.detail },
          detail: note?.detail ?? (fn.implementation === 'source'
            ? 'Helper implemented in VL.' : 'Native operation supplied by the target.'),
          example: note?.example,
        };
      }),
      errors: module.errors.map((error) => ({
        ...error,
        detail: prose?.errors?.find((item) => item.name === error.name)?.detail
          ?? 'Error set returned by this module’s checked operations.',
      })),
    };
  });
  if (new Set(result.map((module) => module.id)).size !== result.length) {
    throw new Error('Duplicate stdlib page IDs; assign unique IDs in src/data/stdlib/*.yaml.');
  }
  return result;
}
