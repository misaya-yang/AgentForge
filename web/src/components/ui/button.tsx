/* eslint-disable react-refresh/only-export-components */
import * as React from "react";
import { Button as AntButton } from "antd";
import { childWithProps } from "./composition";


import { cn } from "@/lib/utils";

const buttonStyles = {
    variants: {
      variant: {
        default: "ui-button-variant-default",
        primary: "ui-button-variant-primary",
        quiet: "ui-button-variant-quiet",
        secondary: "ui-button-variant-secondary",
        outline: "ui-button-variant-outline",
        ghost: "ui-button-variant-ghost",
        destructive: "ui-button-variant-destructive",
        link: "ui-button-variant-link",
      },
      size: {
        default: "ui-button-size-default",
        sm: "ui-button-size-sm",
        lg: "ui-button-size-lg",
        icon: "ui-button-size-icon",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
;

type ButtonVariant = keyof typeof buttonStyles.variants.variant;
type ButtonSize = keyof typeof buttonStyles.variants.size;
function buttonVariants({ variant, size, className }: { variant?: ButtonVariant | null; size?: ButtonSize | null; className?: string } = {}) {
  return cn(
  "ui-button-slot-1", buttonStyles.variants.variant[variant ?? "default"], buttonStyles.variants.size[size ?? "default"], className);
}

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant | null;
  size?: ButtonSize | null;
  asChild?: boolean;
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, color: _color, ...props }, ref) => {
    void _color;
    const classes = buttonVariants({ variant, size, className });
    if (asChild) return childWithProps(props.children, { ...props, className: classes, ref });
    return <AntButton {...props} ref={ref} htmlType={props.type ?? "button"}
      type={variant === "primary" ? "primary" : variant === "link" ? "link" : variant === "ghost" ? "text" : "default"}
      danger={variant === "destructive"} className={classes} />;
  }
);
Button.displayName = "Button";

export { Button, buttonVariants };
