//! Fila sequencial de operações de arquivo.
//!
//! A fila roda em uma thread dedicada (I/O bloqueante) e conversa com a UI por
//! `std::sync::mpsc`: a UI envia [`Op`]s e drena [`QueueEvent`]s sem bloquear.
//! Uma operação é processada por vez, cancelamento é cooperativo entre
//! arquivos e o stack de undo guarda as operações desfazíveis.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::ops::{CompletedOp, Op, OpsError, Progress, execute};

/// Limite padrão de operações no [`UndoStack`].
pub const DEFAULT_UNDO_LIMIT: usize = 50;

/// Tempo ocioso antes de a fila anunciar [`QueueEvent::FinishedQueue`].
const IDLE_GRACE: Duration = Duration::from_millis(100);

/// Resultado de uma operação executada pela fila.
#[derive(Debug)]
pub struct OpResult {
    /// Operação original.
    pub op: Op,
    /// Sucesso (com dados para undo) ou erro.
    pub outcome: Result<CompletedOp, OpsError>,
}

/// Eventos emitidos pela fila para a UI.
#[derive(Debug)]
pub enum QueueEvent {
    /// Uma operação começou.
    Started(Op),
    /// Progresso de uma operação em andamento.
    Progress(Progress),
    /// Uma operação terminou.
    Done(OpResult),
    /// A fila ficou vazia.
    FinishedQueue,
    /// A operação corrente e as pendentes foram canceladas.
    Cancelled,
}

/// Erro ao enfileirar operações.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// A thread da fila encerrou.
    #[error("fila de operações encerrada")]
    Closed,
}

/// Fila sequencial em background.
pub struct OpQueue {
    tx: Option<Sender<Op>>,
    rx: Receiver<QueueEvent>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl OpQueue {
    /// Cria a fila e inicia a thread dedicada.
    pub fn new() -> Self {
        let (op_tx, op_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);

        let worker = thread::Builder::new()
            .name(String::from("arandu-ops"))
            .spawn(move || worker_loop(op_rx, event_tx, worker_cancel))
            .expect("iniciar thread da fila de operações");

        Self {
            tx: Some(op_tx),
            rx: event_rx,
            cancel,
            worker: Some(worker),
        }
    }

    /// Enfileira uma operação.
    pub fn enqueue(&self, op: Op) -> Result<(), QueueError> {
        self.tx
            .as_ref()
            .ok_or(QueueError::Closed)?
            .send(op)
            .map_err(|_| QueueError::Closed)
    }

    /// Pede o cancelamento da operação corrente; as pendentes são descartadas.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Drena o próximo evento, sem bloquear.
    pub fn try_recv(&self) -> Option<QueueEvent> {
        self.rx.try_recv().ok()
    }

    /// Aguarda um evento até `timeout` — usado em testes e para shutdown.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<QueueEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Default for OpQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OpQueue {
    fn drop(&mut self) {
        self.cancel();
        // Fecha o canal: o worker termina após a operação corrente.
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(rx: Receiver<Op>, tx: Sender<QueueEvent>, cancel: Arc<AtomicBool>) {
    let mut idle_announced = false;

    loop {
        let op = if idle_announced {
            match rx.recv() {
                Ok(op) => op,
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(IDLE_GRACE) {
                Ok(op) => op,
                Err(RecvTimeoutError::Timeout) => {
                    let _ = tx.send(QueueEvent::FinishedQueue);
                    idle_announced = true;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        idle_announced = false;

        let _ = tx.send(QueueEvent::Started(op.clone()));

        let progress_tx = tx.clone();
        let on_progress = move |progress: Progress| {
            let _ = progress_tx.send(QueueEvent::Progress(progress));
        };
        let outcome = execute(&op, &on_progress, &cancel);
        let cancelled = matches!(outcome, Err(OpsError::Cancelled));
        let _ = tx.send(QueueEvent::Done(OpResult { op, outcome }));

        if cancelled {
            cancel.store(false, Ordering::SeqCst);
            // Cancela a operação corrente e descarta o que estava pendente.
            while rx.try_recv().is_ok() {}
            let _ = tx.send(QueueEvent::Cancelled);
            let _ = tx.send(QueueEvent::FinishedQueue);
            idle_announced = true;
        }
    }
}

/// Pilha de operações desfazíveis, com limite configurável.
#[derive(Debug)]
pub struct UndoStack {
    items: Vec<CompletedOp>,
    limit: usize,
}

impl UndoStack {
    /// Cria a pilha com `limit` entradas (mínimo 1).
    pub fn new(limit: usize) -> Self {
        Self {
            items: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Empilha uma operação concluída, descartando a mais antiga no limite.
    pub fn push(&mut self, completed: CompletedOp) {
        if self.items.len() >= self.limit {
            self.items.remove(0);
        }
        self.items.push(completed);
    }

    /// Remove e devolve a operação mais recente.
    pub fn pop(&mut self) -> Option<CompletedOp> {
        self.items.pop()
    }

    /// Quantidade de operações desfazíveis.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Se não há nada para desfazer.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Esvazia a pilha.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new(DEFAULT_UNDO_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::{OpQueue, QueueEvent, UndoStack};
    use crate::ops::{CompletedOp, Op, OpKind};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let source = dir.path().join("arquivo.txt");
        fs::write(&source, b"conteudo").expect("cria arquivo");
        (dir, source)
    }

    fn drain_until(queue: &OpQueue, predicate: impl Fn(&QueueEvent) -> bool) -> Vec<QueueEvent> {
        let mut events = Vec::new();
        for _ in 0..100 {
            let Some(event) = queue.recv_timeout(Duration::from_secs(5)) else {
                continue;
            };
            let done = predicate(&event);
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    #[test]
    fn processes_ops_sequentially_and_reports_progress() {
        let (dir, source) = fixture();
        let destination = dir.path().join("destino");
        fs::create_dir(&destination).expect("cria destino");
        let queue = OpQueue::new();
        queue
            .enqueue(Op::new(OpKind::Copy, &source, Some(destination.clone())))
            .expect("enfileira");

        let events = drain_until(&queue, |event| matches!(event, QueueEvent::FinishedQueue));

        let mut started = 0;
        let mut progress = 0;
        let mut done = 0;
        for event in &events {
            match event {
                QueueEvent::Started(_) => started += 1,
                QueueEvent::Progress(update) => {
                    progress += 1;
                    assert_eq!(update.files_done, 1);
                    assert_eq!(update.files_total, 1);
                }
                QueueEvent::Done(result) => {
                    done += 1;
                    assert!(result.outcome.is_ok(), "operação deveria concluir");
                }
                _ => {}
            }
        }
        assert_eq!(started, 1);
        assert_eq!(done, 1);
        assert_eq!(progress, 1);
        assert!(destination.join("arquivo.txt").exists());
    }

    #[test]
    fn finished_queue_is_announced_when_idle() {
        let queue = OpQueue::new();
        let events = drain_until(&queue, |event| matches!(event, QueueEvent::FinishedQueue));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, QueueEvent::FinishedQueue))
        );
    }

    #[test]
    fn enqueue_after_finished_still_works() {
        let (dir, source) = fixture();
        let destination = dir.path().join("destino");
        fs::create_dir(&destination).expect("cria destino");
        let queue = OpQueue::new();
        drain_until(&queue, |event| matches!(event, QueueEvent::FinishedQueue));

        queue
            .enqueue(Op::new(OpKind::Copy, &source, Some(destination)))
            .expect("enfileira depois de ociosa");
        let events = drain_until(&queue, |event| matches!(event, QueueEvent::Done(_)));
        assert!(events.iter().any(|e| matches!(e, QueueEvent::Done(_))));
    }

    #[test]
    fn undo_stack_respects_limit() {
        let mut stack = UndoStack::new(2);
        for index in 0..3 {
            stack.push(CompletedOp {
                kind: OpKind::Rename,
                source: std::path::PathBuf::from(format!("/tmp/a{index}")),
                destination: std::path::PathBuf::from(format!("/tmp/b{index}")),
            });
        }
        assert_eq!(stack.len(), 2);
        assert_eq!(
            stack.pop().expect("último").source,
            std::path::PathBuf::from("/tmp/a2"),
            "a entrada mais antiga é descartada"
        );
    }

    #[test]
    fn default_undo_limit_is_50() {
        let mut stack = UndoStack::default();
        for index in 0..60 {
            stack.push(CompletedOp {
                kind: OpKind::Move,
                source: std::path::PathBuf::from(format!("/tmp/a{index}")),
                destination: std::path::PathBuf::from(format!("/tmp/b{index}")),
            });
        }
        assert_eq!(stack.len(), 50);
    }
}
