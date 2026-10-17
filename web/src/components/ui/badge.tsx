

import { cn } from "@/lib/utils";

const badgeStyles = {
    variants: {
      variant: {
        default: "border-transparent bg-primary text-primary-foreground",
        secondary: "border-transparent bg-secondary text-secondary-foreground",
        outline: "text-foreground",
        destructive:
          "border-transparent bg-destructive text-destructive-foreground",
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
  "inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-semibold transition-colors focus:outline-hidden focus:ring-2 focus:ring-ring focus:ring-offset-2", badgeStyles.variants.variant[variant ?? "default"], className);
}

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement> { variant?: BadgeVariant | null; }

function Badge({ className, variant, ...props }: BadgeProps) {
  return (
    <div className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}

export { Badge };
