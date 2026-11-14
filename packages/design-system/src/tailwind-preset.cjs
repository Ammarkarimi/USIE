// Tailwind preset mapping utility names to the design tokens in tokens.css.
const c = (name) => `rgb(var(--c-${name}) / <alpha-value>)`;
module.exports = {
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        bg: c("bg"), surface: c("surface"), "surface-2": c("surface-2"), border: c("border"),
        fg: c("text"), muted: c("muted"), accent: c("accent"), "accent-fg": c("accent-fg"),
        ok: c("ok"), warn: c("warn"), danger: c("danger"), info: c("info"),
        confirmed: c("confirmed"), probable: c("probable"), inferred: c("inferred"), unknown: c("unknown"),
      },
      fontFamily: {
        sans: ["Inter", "ui-sans-serif", "system-ui", "-apple-system", "Segoe UI", "sans-serif"],
        mono: ["JetBrains Mono", "ui-monospace", "SFMono-Regular", "Menlo", "Consolas", "monospace"],
      },
    },
  },
};
