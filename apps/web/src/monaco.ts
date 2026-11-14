// Monaco is bundled with the application (no CDN): only the editor core,
// the syntax-highlighting grammars and the base editor worker are included.
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/basic-languages/monaco.contribution";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import { loader } from "@monaco-editor/react";

(self as unknown as { MonacoEnvironment: unknown }).MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};
loader.config({ monaco });

const ids: Record<string, string> = {
  python: "python", javascript: "javascript", typescript: "typescript", java: "java", c: "c", cpp: "cpp", rust: "rust",
  go: "go", csharp: "csharp", kotlin: "kotlin", json: "json", yaml: "yaml", markdown: "markdown", html: "html", css: "css",
  scss: "scss", less: "less", sql: "sql", shell: "shell", xml: "xml", dockerfile: "dockerfile", toml: "ini", ruby: "ruby",
  php: "php", swift: "swift", scala: "scala", lua: "lua", r: "r", graphql: "graphql", protobuf: "proto", powershell: "powershell",
};

export function monacoLanguage(path: string, language: string): string {
  if (/\.tsx$/.test(path)) return "typescript";
  return ids[language] ?? "plaintext";
}

export { monaco };
