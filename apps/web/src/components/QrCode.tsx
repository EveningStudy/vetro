// Adapted from T3 Code's components/ui/qr-code.tsx (MIT).
import { memo } from "react";
import { QrCode } from "~/lib/qrCode";

function pathData(qr: ReturnType<typeof QrCode.encodeText>, margin: number) {
  const commands: string[] = [];
  for (let y = 0; y < qr.size; y += 1) {
    let runStart = -1;
    for (let x = 0; x <= qr.size; x += 1) {
      const dark = x < qr.size && qr.getModule(x, y);
      if (dark) {
        if (runStart === -1) runStart = x;
        continue;
      }
      if (runStart === -1) continue;
      commands.push(`M${runStart + margin} ${y + margin}h${x - runStart}v1H${runStart + margin}z`);
      runStart = -1;
    }
  }
  return commands.join("");
}

export const QrCodeSvg = memo(function QrCodeSvg({ value, size = 160, margin = 2 }: { value: string; size?: number; margin?: number }) {
  const qr = QrCode.encodeText(value, QrCode.Ecc.MEDIUM);
  const box = qr.size + margin * 2;
  return (
    <svg viewBox={`0 0 ${box} ${box}`} width={size} height={size} shapeRendering="crispEdges" className="rounded-lg">
      <rect width={box} height={box} fill="#fff" />
      <path d={pathData(qr, margin)} fill="#000" />
    </svg>
  );
});
