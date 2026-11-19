/**
 * Constants for the Assistant page.
 */

import { BookOpen, Code, FileQuestion, TrendingUp, Lightbulb, Brain } from "lucide-react";
import type { SuggestedPrompt } from "./types";

export const SUGGESTED_PROMPTS: SuggestedPrompt[] = [
  {
    icon: <BookOpen className="h-5 w-5" />,
    titleKey: "assistant.suggestions.explain.title",
    promptKey: "assistant.suggestions.explain.prompt",
    categoryKey: "assistant.suggestions.category.learning",
  },
  {
    icon: <Code className="h-5 w-5" />,
    titleKey: "assistant.suggestions.code.title",
    promptKey: "assistant.suggestions.code.prompt",
    categoryKey: "assistant.suggestions.category.development",
  },
  {
    icon: <FileQuestion className="h-5 w-5" />,
    titleKey: "assistant.suggestions.doc.title",
    promptKey: "assistant.suggestions.doc.prompt",
    categoryKey: "assistant.suggestions.category.analysis",
  },
  {
    icon: <TrendingUp className="h-5 w-5" />,
    titleKey: "assistant.suggestions.insights.title",
    promptKey: "assistant.suggestions.insights.prompt",
    categoryKey: "assistant.suggestions.category.analysis",
  },
  {
    icon: <Lightbulb className="h-5 w-5" />,
    titleKey: "assistant.suggestions.brainstorm.title",
    promptKey: "assistant.suggestions.brainstorm.prompt",
    categoryKey: "assistant.suggestions.category.creative",
  },
  {
    icon: <Brain className="h-5 w-5" />,
    titleKey: "assistant.suggestions.solve.title",
    promptKey: "assistant.suggestions.solve.prompt",
    categoryKey: "assistant.suggestions.category.analysis",
  },
];
