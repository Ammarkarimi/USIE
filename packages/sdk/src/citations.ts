// Splitting AI answer text into plain text, inline code and evidence
// citations, so the interface can render citations as links without ever
// interpreting answer text as markup.

export type AnswerSegment =
  | { type: "text"; text: string }
  | { type: "code"; text: string }
  | { type: "citation"; id: string };

const pattern = /`([^`\n]+)`|\[(C\d{1,3})\]/g;

export function segmentAnswer(text: string): AnswerSegment[] {
  const out: AnswerSegment[] = [];
  let last = 0;
  for (const m of text.matchAll(pattern)) {
    const index = m.index ?? 0;
    if (index > last) out.push({ type: "text", text: text.slice(last, index) });
    if (m[1] !== undefined) out.push({ type: "code", text: m[1] });
    else if (m[2] !== undefined) out.push({ type: "citation", id: m[2] });
    last = index + m[0].length;
  }
  if (last < text.length) out.push({ type: "text", text: text.slice(last) });
  return out;
}
