const SECRET_PATTERNS = [
  /\bsk-[A-Za-z0-9_-]{12,}\b/g,
  /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g,
  /\b(?:ghp|github_pat|glpat)-?[A-Za-z0-9_-]{12,}\b/gi,
  /\bAIza[A-Za-z0-9_-]{20,}\b/g,
  /\bBearer\s+[A-Za-z0-9._~+/=-]{12,}\b/gi,
  /\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|client[_-]?secret|secret|password|credential|authorization|cookie|private[_-]?key)\b["']?\s*[:=]\s*["']?[^\s,;"'}]+/gi,
  /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g,
] as const;

export function safeSubAgentText(value: string | undefined, maxLength = 500): string {
  let safe = [...(value ?? "")]
    .map((character) => {
      const code = character.charCodeAt(0);
      return (code < 32 && code !== 9 && code !== 10 && code !== 13) || code === 127
        ? " "
        : character;
    })
    .join("");
  for (const pattern of SECRET_PATTERNS) safe = safe.replace(pattern, "[redacted]");
  return safe.trim().slice(0, maxLength);
}
