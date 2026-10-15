/**
 * Searchable Combobox Component
 *
 * A modern, professional combobox with search filtering, keyboard navigation,
 * and customizable rendering. Uses Portal with fixed positioning for proper
 * z-index layering over all content.
 */

import * as React from "react";
import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown, Search, X, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";

export interface ComboboxOption {
  value: string;
  label: string;
  description?: string;
  icon?: React.ReactNode;
  disabled?: boolean;
  // Allow extra data
  [key: string]: unknown;
}

interface ComboboxProps {
  options: ComboboxOption[];
  value?: string;
  onChange?: (value: string, option: ComboboxOption | undefined) => void;
  placeholder?: string;
  searchPlaceholder?: string;
  emptyText?: string;
  loading?: boolean;
  disabled?: boolean;
  className?: string;
  // Custom filter function
  filterFn?: (option: ComboboxOption, searchQuery: string) => boolean;
  // Custom render for option item
  renderOption?: (option: ComboboxOption, isSelected: boolean) => React.ReactNode;
  // Custom render for trigger display
  renderValue?: (option: ComboboxOption | undefined) => React.ReactNode;
}

export function Combobox({
  options,
  value,
  onChange,
  placeholder = "Select an option...",
  searchPlaceholder = "Search...",
  emptyText = "No results found",
  loading = false,
  disabled = false,
  className,
  filterFn,
  renderOption,
  renderValue,
}: ComboboxProps) {
  const [open, setOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [highlightedIndex, setHighlightedIndex] = useState(0);
  const [dropdownStyle, setDropdownStyle] = useState<React.CSSProperties>({});

  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Find selected option
  const selectedOption = useMemo(
    () => options.find((opt) => opt.value === value),
    [options, value]
  );

  // Default filter function - fuzzy match on label and description
  const defaultFilter = useCallback(
    (option: ComboboxOption, query: string): boolean => {
      const q = query.toLowerCase().trim();
      if (!q) return true;
      const labelMatch = option.label.toLowerCase().includes(q);
      const descMatch = option.description?.toLowerCase().includes(q) ?? false;
      const valueMatch = option.value.toLowerCase().includes(q);
      return labelMatch || descMatch || valueMatch;
    },
    []
  );

  // Filtered options
  const filteredOptions = useMemo(() => {
    const filter = filterFn || defaultFilter;
    return options.filter((opt) => filter(opt, searchQuery));
  }, [options, searchQuery, filterFn, defaultFilter]);

  // Reset highlight when filtered options change
  useEffect(() => {
    setHighlightedIndex(0);
  }, [filteredOptions.length]);

  // Calculate dropdown position - use fixed positioning with viewport coordinates
  const updateDropdownPosition = useCallback(() => {
    if (!triggerRef.current) return;

    const rect = triggerRef.current.getBoundingClientRect();
    const viewportHeight = window.innerHeight;
    const spaceBelow = viewportHeight - rect.bottom;
    const spaceAbove = rect.top;
    const dropdownHeight = 320; // max-height of dropdown

    // Determine if dropdown should open upward or downward
    const openUpward = spaceBelow < dropdownHeight && spaceAbove > spaceBelow;

    setDropdownStyle({
      position: "fixed",
      left: rect.left,
      width: rect.width,
      ...(openUpward
        ? { bottom: viewportHeight - rect.top + 4 }
        : { top: rect.bottom + 4 }),
      zIndex: 99999,
    });
  }, []);

  // Update position when opened and on scroll/resize
  useEffect(() => {
    if (open) {
      updateDropdownPosition();

      const handleUpdate = () => updateDropdownPosition();
      window.addEventListener("scroll", handleUpdate, true);
      window.addEventListener("resize", handleUpdate);

      return () => {
        window.removeEventListener("scroll", handleUpdate, true);
        window.removeEventListener("resize", handleUpdate);
      };
    }
  }, [open, updateDropdownPosition]);

  // Focus input when opened
  useEffect(() => {
    if (open && inputRef.current) {
      // Small delay to ensure portal is mounted
      const timer = setTimeout(() => inputRef.current?.focus(), 10);
      return () => clearTimeout(timer);
    }
  }, [open]);

  // Close on outside click
  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (e: MouseEvent) => {
      const target = e.target as Node;
      const isOutsideContainer = containerRef.current && !containerRef.current.contains(target);
      const isOutsideDropdown = dropdownRef.current && !dropdownRef.current.contains(target);

      if (isOutsideContainer && isOutsideDropdown) {
        setOpen(false);
        setSearchQuery("");
      }
    };

    // Use capture phase to handle clicks before other handlers
    document.addEventListener("mousedown", handleClickOutside, true);
    return () => document.removeEventListener("mousedown", handleClickOutside, true);
  }, [open]);

  // Scroll highlighted item into view
  useEffect(() => {
    if (open && listRef.current) {
      const highlighted = listRef.current.querySelector('[data-highlighted="true"]');
      if (highlighted) {
        highlighted.scrollIntoView({ block: "nearest" });
      }
    }
  }, [highlightedIndex, open]);

  // Keyboard navigation
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
        e.preventDefault();
        setOpen(true);
      }
      return;
    }

    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setHighlightedIndex((prev) =>
          prev < filteredOptions.length - 1 ? prev + 1 : prev
        );
        break;
      case "ArrowUp":
        e.preventDefault();
        setHighlightedIndex((prev) => (prev > 0 ? prev - 1 : 0));
        break;
      case "Enter":
        e.preventDefault();
        if (filteredOptions[highlightedIndex] && !filteredOptions[highlightedIndex].disabled) {
          handleSelect(filteredOptions[highlightedIndex]);
        }
        break;
      case "Escape":
        e.preventDefault();
        setOpen(false);
        setSearchQuery("");
        break;
      case "Tab":
        setOpen(false);
        setSearchQuery("");
        break;
    }
  };

  const handleSelect = (option: ComboboxOption) => {
    onChange?.(option.value, option);
    setOpen(false);
    setSearchQuery("");
  };

  const handleClear = (e: React.MouseEvent) => {
    e.stopPropagation();
    onChange?.("", undefined);
  };

  // Default option renderer
  const defaultRenderOption = (option: ComboboxOption, isSelected: boolean) => (
    <div className="flex items-center gap-3 w-full">
      {option.icon && (
        <span className={cn(
          "shrink-0",
          isSelected ? "text-primary" : "text-muted-foreground"
        )}>
          {option.icon}
        </span>
      )}
      <div className="flex-1 min-w-0">
        <div className={cn(
          "truncate",
          isSelected ? "font-semibold text-foreground" : "font-medium"
        )}>
          {option.label}
        </div>
        {option.description && (
          <div className={cn(
            "text-xs truncate mt-0.5",
            isSelected ? "text-muted-foreground" : "text-muted-foreground/70"
          )}>
            {option.description}
          </div>
        )}
      </div>
      {isSelected && (
        <Check className="h-4 w-4 shrink-0 text-primary" />
      )}
    </div>
  );

  // Default value renderer
  const defaultRenderValue = (option: ComboboxOption | undefined) => {
    if (!option) return <span className="text-muted-foreground">{placeholder}</span>;
    return (
      <div className="flex items-center gap-2 truncate">
        {option.icon && <span className="shrink-0">{option.icon}</span>}
        <span className="truncate">{option.label}</span>
      </div>
    );
  };

  // Render dropdown using Portal with fixed positioning
  const renderDropdown = () => {
    if (!open) return null;

    return createPortal(
      <>
        {/* Backdrop - theme-aware semi-transparent overlay */}
        <div
          className="fixed inset-0 bg-background/60 backdrop-blur-[2px]"
          style={{ zIndex: 99998 }}
          onClick={() => {
            setOpen(false);
            setSearchQuery("");
          }}
        />

        {/* Dropdown Panel */}
        <div
          ref={dropdownRef}
          style={dropdownStyle}
          className={cn(
            // Container
            "rounded-xl overflow-hidden",
            // Border & Shadow - prominent to stand out
            "border border-border/60 ring-1 ring-black/5 dark:ring-white/10",
            "shadow-xl",
            // Solid background - using theme colors
            "bg-popover",
            // Animation
            "animate-in fade-in-0 zoom-in-[0.98] duration-150"
          )}
        >
          {/* Search Input Area */}
          <div className="p-3 bg-linear-to-b from-muted/40 to-muted/20 border-b border-border/40">
            <div className="relative">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground/60" />
              <input
                ref={inputRef}
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={searchPlaceholder}
                className={cn(
                  "w-full h-10 pl-9 pr-9 rounded-lg text-sm",
                  "bg-background",
                  "border border-border/60",
                  "outline-hidden",
                  "placeholder:text-muted-foreground/50",
                  "focus:border-primary/50 focus:ring-2 focus:ring-primary/20",
                  "transition-all duration-150"
                )}
              />
              {searchQuery && (
                <button
                  onClick={() => setSearchQuery("")}
                  className="absolute right-2.5 top-1/2 -translate-y-1/2 p-1 rounded-md hover:bg-muted transition-colors"
                >
                  <X className="h-3.5 w-3.5 text-muted-foreground" />
                </button>
              )}
            </div>
          </div>

          {/* Options List */}
          <div
            ref={listRef}
            className="max-h-[240px] overflow-y-auto p-2 bg-popover"
          >
            {loading ? (
              <div className="flex items-center justify-center py-8 text-muted-foreground">
                <Loader2 className="h-5 w-5 animate-spin mr-2" />
                <span className="text-sm">Loading...</span>
              </div>
            ) : filteredOptions.length === 0 ? (
              <div className="py-8 text-center text-sm text-muted-foreground">
                {emptyText}
              </div>
            ) : (
              <div className="space-y-0.5">
                {filteredOptions.map((option, index) => {
                  const isSelected = option.value === value;
                  const isHighlighted = index === highlightedIndex;

                  return (
                    <div
                      key={option.value}
                      data-highlighted={isHighlighted}
                      onClick={() => !option.disabled && handleSelect(option)}
                      onMouseEnter={() => setHighlightedIndex(index)}
                      className={cn(
                        "relative flex cursor-pointer select-none items-center",
                        "rounded-lg px-3 py-2.5 text-sm outline-hidden",
                        "transition-all duration-100",
                        // Base state
                        "text-foreground",
                        // Hover state
                        !isSelected && !isHighlighted && "hover:bg-muted/60",
                        // Highlighted state (keyboard navigation)
                        isHighlighted && !isSelected && "bg-muted/80",
                        // Selected state - subtle but clear
                        isSelected && "bg-primary/10 border border-primary/20",
                        // Disabled
                        option.disabled && "pointer-events-none opacity-50"
                      )}
                    >
                      {renderOption
                        ? renderOption(option, isSelected)
                        : defaultRenderOption(option, isSelected)}
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* Footer hint */}
          <div className="px-3 py-2 bg-muted/30 border-t border-border/40">
            <div className="flex items-center justify-between text-[10px] text-muted-foreground/60">
              <span>↑↓ Navigate</span>
              <span>↵ Select</span>
              <span>Esc Close</span>
            </div>
          </div>
        </div>
      </>,
      document.body
    );
  };

  return (
    <div ref={containerRef} className={cn("relative", className)}>
      {/* Trigger Button */}
      <button
        ref={triggerRef}
        type="button"
        onClick={() => !disabled && setOpen(!open)}
        onKeyDown={handleKeyDown}
        disabled={disabled}
        className={cn(
          "flex h-10 w-full items-center justify-between",
          "rounded-lg border border-input bg-background px-3 py-2 text-sm",
          "ring-offset-background",
          "focus:outline-hidden focus:ring-2 focus:ring-ring focus:ring-offset-2",
          "disabled:cursor-not-allowed disabled:opacity-50",
          "transition-all duration-200",
          "hover:border-primary/50",
          open && "ring-2 ring-primary/30 border-primary/50"
        )}
      >
        <span className="flex-1 text-left truncate">
          {renderValue ? renderValue(selectedOption) : defaultRenderValue(selectedOption)}
        </span>
        <div className="flex items-center gap-1 ml-2">
          {value && !disabled && (
            <span
              onClick={handleClear}
              className="p-1 rounded-md hover:bg-muted cursor-pointer transition-colors"
            >
              <X className="h-3.5 w-3.5 text-muted-foreground" />
            </span>
          )}
          <ChevronDown
            className={cn(
              "h-4 w-4 text-muted-foreground transition-transform duration-200",
              open && "rotate-180"
            )}
          />
        </div>
      </button>

      {/* Dropdown rendered via Portal with fixed positioning */}
      {renderDropdown()}
    </div>
  );
}
