import { defineCollection } from 'astro:content';
import { file } from 'astro/loaders';
import { z } from 'astro/zod';

// VL standard library catalog (`src/data/stdlib.yaml`). Every `std` docs
// page, the section overview, and the sidebar are generated from this
// collection, so prose and structure cannot drift apart.
const stdlibParam = z.object({
  name: z.string(),
  type: z.string(),
  detail: z.string().optional(),
});

const stdlibFunction = z.object({
  name: z.string(),
  // Structured signature. Absent while the language has not fixed the
  // function's type surface — the compiler only knows export names
  // (plus: `print` takes one string, every external call types as `i64`).
  params: z.array(stdlibParam).optional(),
  returns: z
    .object({ type: z.string(), detail: z.string().optional() })
    .optional(),
  detail: z.string(),
  example: z.string().optional(),
});

const stdlib = defineCollection({
  loader: file('src/data/stdlib.yaml'),
  schema: z.object({
    id: z.string(),
    order: z.number(),
    module: z.string(),
    summary: z.string(),
    title: z.string(),
    description: z.string(),
    intro: z.string(),
    import_example: z.string(),
    functions: z.array(stdlibFunction),
    outro: z.string().optional(),
  }),
});

export const collections = { stdlib };
