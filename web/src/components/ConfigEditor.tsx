export function ConfigEditor({
  value,
  onChange,
  height = 400,
}: {
  value: string;
  onChange: (v: string) => void;
  height?: number;
}) {
  return (
    <textarea
      className="w-full rounded-md border bg-background p-3 font-mono text-sm"
      style={{ height }}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      spellCheck={false}
    />
  );
}
