

import { cn } from "@/lib/utils";

const badgeStyles = {
    variants: {
      variant: {
        default: "ui-badge-variant-default",
        secondary: "ui-badge-variant-secondary",
        outline: "text-foreground",
        destructive:
          "ui-badge-variant-destructive",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
;

type BadgeVariant = keyof typeof badgeStyles.variants.variant;
function badgeVariants({ variant, className }: { variant?: BadgeVariant | null; className?: string } = {}) {
  return cn(
  "ui-badge-slot-1", badgeStyles.variants.variant[variant ?? "default"], className);
}

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement> { variant?: BadgeVariant | null; }

function Badge({ className, variant, ...props }: BadgeProps) {
  return (
    <div className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}

export { Badge };
