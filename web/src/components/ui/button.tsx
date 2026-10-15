/* eslint-disable react-refresh/only-export-components */
import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex items-center justify-center whitespace-nowrap rounded-md text-sm font-medium transition-[color,background-color,border-color,box-shadow,transform] duration-150 focus-visible:outline-hidden focus-visible:ring-[3px] focus-visible:ring-ring/20 focus-visible:ring-offset-0 disabled:pointer-events-none disabled:opacity-50 disabled:shadow-none active:translate-y-px",
  {
    variants: {
      variant: {
        default: "bg-primary/8 text-primary border border-primary/15 hover:bg-primary/[0.14] hover:border-primary/25 dark:bg-primary/12 dark:border-primary/20 dark:hover:bg-primary/20 dark:hover:border-primary/30",
        primary: "border border-primary bg-primary text-primary-foreground shadow-[0_1px_2px_hsl(var(--foreground)/0.08)] hover:bg-primary/90 hover:border-primary/90 disabled:border-border disabled:bg-muted disabled:text-muted-foreground disabled:opacity-100",
        quiet: "border border-transparent bg-muted/55 text-foreground hover:bg-muted",
        secondary: "bg-secondary text-secondary-foreground border border-border hover:bg-secondary/70 hover:border-border/80",
        outline: "border border-input bg-background hover:bg-muted hover:text-foreground",
        ghost: "hover:bg-muted hover:text-foreground",
        destructive: "bg-destructive/8 text-destructive border border-destructive/15 hover:bg-destructive/[0.14] hover:border-destructive/25 dark:bg-destructive/12 dark:border-destructive/20 dark:hover:bg-destructive/20",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 rounded-md px-3 text-xs",
        lg: "h-10 rounded-md px-6",
        icon: "h-9 w-9",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean;
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : "button";
    return (
      <Comp
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        {...props}
      />
    );
  }
);
Button.displayName = "Button";

export { Button, buttonVariants };
