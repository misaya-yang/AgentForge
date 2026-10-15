/**
 * SafeResponsiveChart - A wrapper for Recharts ResponsiveContainer
 * that handles edge cases where the container dimensions are not available
 * (e.g., during initial render, SSR, or when parent container is hidden).
 *
 * This prevents the "The width(-1) and height(-1) of chart should be greater than 0" error.
 */
import {
  Children,
  cloneElement,
  isValidElement,
  useRef,
  useEffect,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";

interface SafeResponsiveChartProps {
  children: ReactNode;
  width?: string | number;
  height?: string | number;
  minWidth?: number;
  minHeight?: number;
  className?: string;
  style?: React.CSSProperties;
}

interface ChartDimensionProps {
  width?: number;
  height?: number;
}

export function SafeResponsiveChart({
  children,
  width = "100%",
  height = "100%",
  minWidth = 100,
  minHeight = 100,
  className,
  style,
}: SafeResponsiveChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<{ width: number; height: number } | null>(null);

  useEffect(() => {
    const checkDimensions = () => {
      if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        if (rect.width > 0 && rect.height > 0) {
          const nextSize = {
            width: Math.max(Math.floor(rect.width), minWidth),
            height: Math.max(Math.floor(rect.height), minHeight),
          };
          setSize((current) =>
            current?.width === nextSize.width && current?.height === nextSize.height
              ? current
              : nextSize,
          );
        }
      }
    };

    // Check immediately
    checkDimensions();

    // Also check after a small delay to handle layout shifts
    const timeoutId = setTimeout(checkDimensions, 100);

    // Use ResizeObserver for more reliable detection
    const resizeObserver = new ResizeObserver(checkDimensions);
    if (containerRef.current) {
      resizeObserver.observe(containerRef.current);
    }

    return () => {
      clearTimeout(timeoutId);
      resizeObserver.disconnect();
    };
  }, [minWidth, minHeight]);

  const chartChildren = size
    ? Children.map(children, (child) =>
        isValidElement<ChartDimensionProps>(child)
          ? cloneElement(child as ReactElement<ChartDimensionProps>, {
              width: size.width,
              height: size.height,
            })
          : child,
      )
    : null;

  return (
    <div
      ref={containerRef}
      className={className}
      style={{
        width: typeof width === "number" ? `${width}px` : width,
        height: typeof height === "number" ? `${height}px` : height,
        minWidth,
        minHeight,
        ...style,
      }}
    >
      {size ? (
        chartChildren
      ) : (
        <div
          style={{
            width: "100%",
            height: "100%",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: "hsl(var(--muted-foreground))",
            fontSize: 12,
          }}
        >
          Loading chart...
        </div>
      )}
    </div>
  );
}

export default SafeResponsiveChart;
