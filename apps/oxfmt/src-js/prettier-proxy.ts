import Tinypool from "tinypool";
import type { WorkerData, FormatEmbeddedCodeArgs, FormatFileArgs } from "./prettier-worker.ts";

// Worker pool for parallel Prettier formatting
// Used by each exported function
let pool: Tinypool | null = null;

type SetupResult = string[];
let setupCache: SetupResult | null = null;

// ---

/**
 * Setup Prettier configuration.
 * NOTE: Called from Rust via NAPI ThreadsafeFunction with FnArgs
 * @param configJSON - Prettier configuration as JSON string
 * @param numThreads - Number of worker threads to use (same as Rayon thread count)
 * @returns Array of loaded plugin's `languages` info
 * */
export async function setupConfig(configJSON: string, numThreads: number): Promise<SetupResult> {
  // NOTE: When called from CLI, it's only called once at the beginning.
  // However, when called via API, like `format(fileName, code)`, it may be called multiple times.
  // Therefore, allow it by returning cached result.
  if (setupCache !== null) return setupCache;

  const workerData: WorkerData = {
    // SAFETY: Always valid JSON constructed by Rust side
    prettierConfig: JSON.parse(configJSON),
  };

  // Initialize worker pool for parallel Prettier formatting
  // Pass config via workerData so all workers get it on initialization
  pool = new Tinypool({
    filename: new URL("./prettier-worker.js", import.meta.url).href,
    minThreads: numThreads,
    maxThreads: numThreads,
    workerData,
  });

  // TODO: Plugins support
  // - Read `plugins` field
  // - Load plugins dynamically and parse `languages` field
  // - Map file extensions and filenames to Prettier parsers
  setupCache = [];

  return setupCache;
}

// ---

// Map template tag names to Prettier parsers
const TAG_TO_PARSER: Record<string, string> = {
  // CSS
  css: "css",
  styled: "css",
  // GraphQL
  gql: "graphql",
  graphql: "graphql",
  // HTML
  html: "html",
  // Markdown
  md: "markdown",
  markdown: "markdown",
};

/**
 * Format embedded code using Prettier.
 * NOTE: Called from Rust via NAPI ThreadsafeFunction with FnArgs
 * @param tagName - The template tag name (e.g., "css", "gql", "html")
 * @param code - The code to format
 * @returns Formatted code
 */
export async function formatEmbeddedCode(tagName: string, code: string): Promise<string> {
  const parser = TAG_TO_PARSER[tagName];

  // Unknown tag, return original code
  if (!parser) {
    return code;
  }

  return pool!.run({ parser, code } satisfies FormatEmbeddedCodeArgs, {
    name: "formatEmbeddedCode",
  });
}

// ---

/**
 * Format whole file content using Prettier.
 * NOTE: Called from Rust via NAPI ThreadsafeFunction with FnArgs
 * @param parserName - The parser name
 * @param fileName - The file name (e.g., "package.json")
 * @param code - The code to format
 * @returns Formatted code
 */
export async function formatFile(
  parserName: string,
  fileName: string,
  code: string,
): Promise<string> {
  return pool!.run({ parserName, fileName, code } satisfies FormatFileArgs, {
    name: "formatFile",
  });
}

// ---

// Optional: Tailwind sorter (initialized lazily on first use)
// Import types only to avoid runtime error if plugin is not installed
import type {
  BatchSortContext,
  PluginOptions,
  TransformerEnv,
} from "prettier-plugin-tailwindcss";

let tailwindSorter: BatchSortContext | null = null;
let tailwindSorterInitialized = false;

// Store Tailwind options set via setTailwindOptions
let storedTailwindOptions: PluginOptions | null = null;

/**
 * Tailwind CSS options passed from the format API.
 * These match the options from `prettier-plugin-tailwindcss`.
 */
export interface TailwindOptions {
  tailwindConfig?: string;
  tailwindStylesheet?: string;
  tailwindFunctions?: string[];
  tailwindAttributes?: string[];
  tailwindPreserveWhitespace?: boolean;
  tailwindPreserveDuplicates?: boolean;
}

/**
 * Set Tailwind CSS options for class sorting.
 * Called from the format function before processing.
 * @param options - Tailwind options or undefined to clear
 */
export function setTailwindOptions(options: TailwindOptions | undefined): void {
  if (options) {
    storedTailwindOptions = {
      tailwindConfig: options.tailwindConfig,
      tailwindStylesheet: options.tailwindStylesheet,
      tailwindFunctions: options.tailwindFunctions,
      tailwindAttributes: options.tailwindAttributes,
      tailwindPreserveWhitespace: options.tailwindPreserveWhitespace,
      tailwindPreserveDuplicates: options.tailwindPreserveDuplicates,
    };
  } else {
    storedTailwindOptions = null;
  }
  // Reset sorter so it gets re-initialized with new options
  tailwindSorter = null;
  tailwindSorterInitialized = false;
}

/**
 * Create a batch sorter context using the patched plugin's exported functions.
 * This implements the BatchSortContext interface locally.
 */
async function createBatchSorter(): Promise<BatchSortContext> {
  // Dynamic import to get the patched plugin's exports
  const { getTailwindConfig, sortClasses } = await import("prettier-plugin-tailwindcss");

  // Build options for getTailwindConfig
  const configOptions: Partial<PluginOptions & { filepath?: string }> = {
    filepath: process.cwd(),
    ...storedTailwindOptions,
  };

  // Load Tailwind context with options
  const context = await getTailwindConfig(configOptions);

  // Create transformer env with stored options
  const env: TransformerEnv = {
    context,
    options: storedTailwindOptions ?? {},
  };

  return {
    sortClasses(classes: string[]): string[] {
      return classes.map((classStr) => {
        try {
          return sortClasses(classStr, { env });
        } catch {
          // Failed to sort, return original
          return classStr;
        }
      });
    },
  };
}

/**
 * Process Tailwind CSS classes found in JSX attributes.
 * NOTE: Called from Rust via NAPI ThreadsafeFunction
 * @param classes - Array of class strings found in JSX class/className attributes
 * @returns Array of sorted class strings (same order/length as input)
 */
export async function processTailwindClasses(classes: string[]): Promise<string[]> {
  // Initialize sorter on first call (lazy)
  if (!tailwindSorterInitialized) {
    tailwindSorterInitialized = true;
    try {
      tailwindSorter = await createBatchSorter();
    } catch {
      // Plugin not installed or failed to initialize - sorting will be skipped
      tailwindSorter = null;
    }
  }

  // If sorter not available, return original classes
  if (!tailwindSorter) {
    return classes;
  }

  // Sort all classes
  return tailwindSorter.sortClasses(classes);
}
