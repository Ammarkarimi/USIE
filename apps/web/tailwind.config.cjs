module.exports = {
  presets: [require("../../packages/design-system/src/tailwind-preset.cjs")],
  content: ["./index.html", "./src/**/*.{ts,tsx}", "../../packages/design-system/src/**/*.{ts,tsx}"],
};
