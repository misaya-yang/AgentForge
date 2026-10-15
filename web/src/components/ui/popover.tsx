import * as React from "react";
import { createPortal } from "react-dom";

import { cn } from "@/lib/utils";

interface PopoverContextValue {
  open: boolean;
  setOpen: (open: boolean) => void;
  // React 19: useRef<T>(null) yields RefObject<T | null>.
  triggerRef: React.RefObject<HTMLButtonElement | null>;
  contentRef: React.RefObject<HTMLDivElement | null>;
}

const PopoverContext = React.createContext<PopoverContextValue | null>(null);

function usePopoverContext() {
  const context = React.useContext(PopoverContext);
  if (!context) {
    throw new Error("Popover components must be used within a Popover");
  }
  return context;
}

interface PopoverProps {
  children: React.ReactNode;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

const Popover = ({ children, open: controlledOpen, onOpenChange }: PopoverProps) => {
  const [internalOpen, setInternalOpen] = React.useState(false);
  const triggerRef = React.useRef<HTMLButtonElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);

  const open = controlledOpen !== undefined ? controlledOpen : internalOpen;
  const setOpen = React.useCallback(
    (value: boolean) => {
      if (controlledOpen === undefined) {
        setInternalOpen(value);
      }
      onOpenChange?.(value);
    },
    [controlledOpen, onOpenChange]
  );

  // Close on click outside
  React.useEffect(() => {
    if (!open) return;

    const handleClickOutside = (e: MouseEvent) => {
      const target = e.target as Node;
      // Don't close if clicking on trigger or inside content
      if (triggerRef.current?.contains(target)) return;
      if (contentRef.current?.contains(target)) return;
      setOpen(false);
    };

    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };

    // Use 'click' instead of 'mousedown' to allow onClick handlers to fire first
    document.addEventListener("click", handleClickOutside);
    document.addEventListener("keydown", handleEscape);

    return () => {
      document.removeEventListener("click", handleClickOutside);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [open, setOpen]);

  return (
    <PopoverContext.Provider value={{ open, setOpen, triggerRef, contentRef }}>
      {children}
    </PopoverContext.Provider>
  );
};

interface PopoverTriggerProps {
  children: React.ReactElement;
  asChild?: boolean;
}

const PopoverTrigger = React.forwardRef<HTMLButtonElement, PopoverTriggerProps>(
  ({ children, asChild }, ref) => {
    const { open, setOpen, triggerRef } = usePopoverContext();
    const setTriggerNode = React.useCallback(
      (node: HTMLButtonElement | null) => {
        triggerRef.current = node;
        if (typeof ref === "function") {
          ref(node);
        } else if (ref) {
          ref.current = node;
        }
      },
      [ref, triggerRef]
    );

    if (asChild && React.isValidElement(children)) {
      const childProps = children.props as {
        onClick?: (event: React.MouseEvent) => void;
        ref?: React.Ref<HTMLButtonElement>;
      };
      return React.cloneElement(children as React.ReactElement<typeof childProps>, {
        ref: setTriggerNode,
        onClick: (e: React.MouseEvent) => {
          childProps.onClick?.(e);
          setOpen(!open);
        },
      });
    }

    return (
      <button ref={setTriggerNode} onClick={() => setOpen(!open)}>
        {children}
      </button>
    );
  }
);
PopoverTrigger.displayName = "PopoverTrigger";

interface PopoverContentProps extends React.HTMLAttributes<HTMLDivElement> {
  align?: "start" | "center" | "end";
  side?: "top" | "bottom" | "left" | "right";
  sideOffset?: number;
}

const PopoverContent = React.forwardRef<HTMLDivElement, PopoverContentProps>(
  ({ className, align = "center", side = "bottom", sideOffset = 8, children, ...props }, ref) => {
    const { open, triggerRef, contentRef } = usePopoverContext();
    const [position, setPosition] = React.useState({ top: 0, left: 0 });

    React.useEffect(() => {
      if (!open || !triggerRef.current) return;

      const trigger = triggerRef.current.getBoundingClientRect();
      const content = contentRef.current;

      let top = 0;
      let left = 0;

      if (side === "bottom") {
        top = trigger.bottom + sideOffset;
      } else if (side === "top") {
        top = trigger.top - sideOffset - (content?.offsetHeight || 0);
      }

      if (align === "start") {
        left = trigger.left;
      } else if (align === "center") {
        left = trigger.left + trigger.width / 2 - (content?.offsetWidth || 0) / 2;
      } else if (align === "end") {
        left = trigger.right - (content?.offsetWidth || 0);
      }

      // Clamp to viewport
      left = Math.max(8, Math.min(left, window.innerWidth - (content?.offsetWidth || 0) - 8));
      top = Math.max(8, Math.min(top, window.innerHeight - (content?.offsetHeight || 0) - 8));

      setPosition({ top, left });
    }, [open, align, side, sideOffset, triggerRef, contentRef]);

    if (!open) return null;

    return createPortal(
      <div
        ref={(node) => {
          (contentRef as React.MutableRefObject<HTMLDivElement | null>).current = node;
          if (typeof ref === "function") ref(node);
          else if (ref) ref.current = node;
        }}
        className={cn(
          "z-50 min-w-32 overflow-hidden rounded-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 p-2 text-popover-foreground shadow-xl",
          "animate-in fade-in-0 zoom-in-95",
          className
        )}
        style={{
          position: "fixed",
          top: position.top,
          left: position.left,
        }}
        {...props}
      >
        {children}
      </div>,
      document.body
    );
  }
);
PopoverContent.displayName = "PopoverContent";

export { Popover, PopoverTrigger, PopoverContent };
