import { defineCollection } from 'astro:content';
import { file } from 'astro/loaders';
import { z } from 'astro/zod';

// Compiler-derived catalog enriched with `src/data/stdlib.yaml` prose. Every `std` docs
// page, the section overview, and the sidebar are generated from this
// collection, so prose and structure cannot drift apart.
const stdlibParam = z.object({
  name: z.string(),
  type: z.string(),
  detail: z.string().optional(),
});

const stdlibFunction = z.object({
  name: z.string(),
  type_params: z.array(z.string()).optional(),
  // Structured signature. Mirrors the compiler-owned extern signatures
  // (`vl-codegen::modules`); every export is fully typed.
  params: z.array(stdlibParam),
  returns: z.object({ type: z.string(), detail: z.string().optional() }),
  detail: z.string(),
  example: z.string().optional(),
});

const stdlibError = z.object({
  name: z.string(),
  variants: z.array(z.string()),
  detail: z.string(),
});

const stdlib = defineCollection({
  loader: file('src/data/stdlib.generated.json'),
  schema: z.object({
    id: z.string(),
    order: z.number(),
    module: z.string(),
    source: z.string().optional(),
    summary: z.string(),
    title: z.string(),
    description: z.string(),
    intro: z.string(),
    import_example: z.string(),
    functions: z.array(stdlibFunction),
    errors: z.array(stdlibError).optional(),
    outro: z.string().optional(),
  }),
});

export const collections = { stdlib };
