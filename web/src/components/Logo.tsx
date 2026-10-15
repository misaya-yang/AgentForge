export function Logo({ collapsed, textClassName }: { collapsed: boolean; textClassName?: string }) {
    return (
        <div className="flex items-center gap-3 select-none group">
            <div className="relative flex items-center justify-center w-9 h-9">
                <img
                    src="/ai-gateway-logo.png"
                    alt="AI Platform Logo"
                    className="w-full h-full object-contain transition-transform duration-500 group-hover:scale-105"
                />
            </div>

            {!collapsed && (
                <div className="flex flex-col justify-center animate-in fade-in slide-in-from-left-2 duration-300 ml-1">
                    <span
                        className={textClassName || "text-[15px] font-semibold text-foreground dark:text-white leading-none"}
                        style={{ letterSpacing: "-0.2px" }}
                    >
                        AI Gateway
                    </span>
                </div>
            )}
        </div>
    );
}
