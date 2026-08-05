import type { Plugin } from "vite";

/**
 * Broadsheet's vendored stylesheet (src/styles/broadsheet.css) ships a
 * `@import url('https://fonts.googleapis.com/...')` line. This console is
 * local-first: nothing may ever hit the network, at dev or build time. This
 * plugin strips any `@import` of a remote (http/https) URL from that one
 * file's source before Vite processes it, so the byte-identical vendored
 * copy on disk never has to be hand-edited.
 */
const REMOTE_IMPORT_RE = /@import\s+url\(\s*['"]?https?:\/\/[^)'"]*['"]?\s*\)\s*;?/gi;

export function stripRemoteImports(): Plugin {
  return {
    name: "strip-remote-imports",
    enforce: "pre",
    transform(code, id) {
      if (!id.endsWith("broadsheet.css")) return null;
      if (!REMOTE_IMPORT_RE.test(code)) return null;
      REMOTE_IMPORT_RE.lastIndex = 0;
      return {
        code: code.replace(REMOTE_IMPORT_RE, ""),
        map: null,
      };
    },
  };
}
