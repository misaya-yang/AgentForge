import { motion } from "framer-motion";
import { cn } from "@/lib/utils";
import { Bot, User } from "lucide-react";

export function MessageAvatar({ isUser }: { isUser: boolean }) {
  return (
    <motion.div
      initial={{ scale: 0.85, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      transition={{ duration: 0.25, ease: "easeOut" }}
      className="relative"
    >
      {/* Subtle glow */}
      <div
        className={cn(
          "absolute inset-0 rounded-xl blur-md opacity-30",
          isUser
            ? "bg-linear-to-br from-emerald-400 to-teal-500"
            : "bg-linear-to-br from-blue-500 to-cyan-500"
        )}
      />

      {/* Avatar */}
      <div
        className={cn(
          "relative flex h-9 w-9 shrink-0 items-center justify-center rounded-xl",
          "shadow-md transition-transform duration-200 hover:scale-105",
          isUser
            ? "bg-linear-to-br from-emerald-500 to-teal-600 text-white shadow-emerald-500/20"
            : "bg-linear-to-br from-blue-500 to-cyan-600 text-white shadow-blue-500/20"
        )}
      >
        {isUser ? (
          <User className="h-4 w-4" strokeWidth={2.5} />
        ) : (
          <Bot className="h-4 w-4" strokeWidth={2.5} />
        )}
      </div>
    </motion.div>
  );
}
