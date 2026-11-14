// Applies the theme before first paint to avoid a flash. Kept as a file (not
// inline) so the Content-Security-Policy can forbid inline scripts.
(function () {
  var pref = null;
  try { pref = localStorage.getItem("usie.theme"); } catch (e) {}
  var dark = pref === "dark" || (pref !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  if (dark) document.documentElement.classList.add("dark");
})();
