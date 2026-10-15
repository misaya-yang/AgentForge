import { motion, useReducedMotion } from "framer-motion";
import { Bot } from "lucide-react";
import { useTranslation } from "react-i18next";

export function WelcomeScreen() {
  const { t } = useTranslation();
  const shouldReduceMotion = useReducedMotion();

  return (
    <motion.div
      initial={shouldReduceMotion ? false : { opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: shouldReduceMotion ? 0 : 0.22, ease: "easeOut" }}
      className="flex min-h-[50vh] flex-col items-center justify-center text-center"
    >
      <div className="mb-7">
        <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary/10 dark:bg-primary/15">
          <Bot className="h-10 w-10 text-primary" />
        </div>
      </div>

      <h2 className="mb-2 text-2xl font-semibold tracking-tight text-foreground">
        {t("assistant.welcomeTitle", "How can I help you today?")}
      </h2>
      <p className="mb-4 max-w-md text-sm leading-relaxed text-muted-foreground">
        {t(
          "assistant.welcomeDesc",
          "Select a model and knowledge bases, then send a message to begin."
        )}
      </p>
      <p className="max-w-md text-xs leading-relaxed text-muted-foreground/70">
        {t(
          "assistant.welcomeWhy",
          "This assistant talks to the model services you configure in the console."
        )}
      </p>
    </motion.div>
  );
}
