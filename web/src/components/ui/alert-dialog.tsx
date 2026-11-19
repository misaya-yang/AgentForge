import * as React from "react";
import { Dialog, DialogTrigger, DialogPortal, DialogOverlay, DialogContent, DialogHeader, DialogFooter, DialogTitle, DialogDescription, DialogClose } from "./dialog";
import { buttonVariants } from "./button";
import { cn } from "@/lib/utils";
function AlertDialog(props: React.ComponentProps<typeof Dialog>) { return <Dialog {...props} alert />; }
const AlertDialogTrigger = DialogTrigger;
const AlertDialogPortal = DialogPortal;
const AlertDialogOverlay = DialogOverlay;
function AlertDialogContent(props: React.ComponentProps<typeof DialogContent>) { return <DialogContent {...props} showCloseButton={false} />; }
const AlertDialogHeader = DialogHeader;
const AlertDialogFooter = DialogFooter;
const AlertDialogTitle = DialogTitle;
const AlertDialogDescription = DialogDescription;
const AlertDialogAction = React.forwardRef<HTMLButtonElement, React.ComponentPropsWithoutRef<typeof DialogClose>>(({ className, ...props }, ref) => <DialogClose {...props} ref={ref} className={cn(buttonVariants(), className)} />);
AlertDialogAction.displayName = "AlertDialogAction";
const AlertDialogCancel = React.forwardRef<HTMLButtonElement, React.ComponentPropsWithoutRef<typeof DialogClose>>(({ className, ...props }, ref) => <DialogClose {...props} ref={ref} className={cn(buttonVariants({ variant: "outline" }), "ui-alert-dialog-slot-1", className)} autoFocus />);
AlertDialogCancel.displayName = "AlertDialogCancel";
export { AlertDialog, AlertDialogTrigger, AlertDialogPortal, AlertDialogOverlay, AlertDialogContent, AlertDialogHeader, AlertDialogFooter, AlertDialogTitle, AlertDialogDescription, AlertDialogAction, AlertDialogCancel };
