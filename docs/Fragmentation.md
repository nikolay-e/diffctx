# Архитектура универсальной системы фрагментации для treemapper

## Проблема

Текущая реализация в `fragments.py` использует кастомные эвристики:
- `_compute_bracket_balance` — ручной парсинг скобок
- `_find_sentence_boundary` — regex для точек/вопросов
- `_find_indent_safe_end_line` — проверка отступов
- `_find_balanced_end_line` — поиск закрывающих скобок

Эти решения работают, но имеют ограничения:
1. Не учитывают строки/комментарии корректно
2. Не понимают семантику языка
3. Дублируют функциональность существующих библиотек

## Архитектурное решение: Strategy Pattern + Fallback Chain

```
┌─────────────────────────────────────────────────────────────┐
│                    FragmentationEngine                       │
│  ┌─────────────────────────────────────────────────────────┐│
│  │                  FormatDetector                         ││
│  │  (определяет тип файла по расширению/содержимому)       ││
│  └─────────────────────────────────────────────────────────┘│
│                            │                                 │
│                            ▼                                 │
│  ┌─────────────────────────────────────────────────────────┐│
│  │              StrategySelector                           ││
│  │  (выбирает оптимальную стратегию для формата)           ││
│  └─────────────────────────────────────────────────────────┘│
│                            │                                 │
│     ┌──────────────────────┼──────────────────────┐         │
│     ▼                      ▼                      ▼         │
│ ┌────────┐           ┌────────┐            ┌────────┐       │
│ │ Code   │           │ Markup │            │ Plain  │       │
│ │Strategy│           │Strategy│            │Strategy│       │
│ └────────┘           └────────┘            └────────┘       │
│     │                    │                      │           │
│     ▼                    ▼                      ▼           │
│ tree-sitter         mistune/lxml           pySBD/NLTK      │
└─────────────────────────────────────────────────────────────┘
```

## Категории контента и рекомендуемые библиотеки

### 1. Код (Programming Languages)

| Формат | Библиотека | Причина выбора |
|--------|------------|----------------|
| Python, JS, TS, Go, Rust, Java, C/C++, etc. | **tree-sitter** | Парсер промышленного качества, 50+ языков, точные позиции узлов AST |
| Только Python | `ast` (stdlib) | Без зависимостей, но только Python |

**tree-sitter преимущества:**
- Инкрементальный парсинг
- Толерантен к синтаксическим ошибкам
- Даёт CST с точными byte/line позициями
- Query language для поиска узлов

```python
# Пример использования tree-sitter
import tree_sitter_python as tspython
from tree_sitter import Language, Parser

parser = Parser(Language(tspython.language()))
tree = parser.parse(code.encode('utf-8'))

# Получаем все функции/классы
def extract_definitions(node, code_bytes):
    fragments = []
    for child in node.children:
        if child.type in ('function_definition', 'class_definition'):
            start = child.start_point[0]  # line
            end = child.end_point[0]
            content = code_bytes[child.start_byte:child.end_byte].decode()
            fragments.append(Fragment(
                start_line=start + 1,
                end_line=end + 1,
                kind=child.type.replace('_definition', ''),
                content=content
            ))
        fragments.extend(extract_definitions(child, code_bytes))
    return fragments
```

**Готовые обёртки:**
- **astchunk** — готовый AST-based chunker (Python, Java, TypeScript, C#)
- **code_ast** — visitor/transformer паттерны поверх tree-sitter
- **LlamaIndex CodeSplitter** — RAG-ready chunking

### 2. Markdown

| Библиотека | Применение |
|------------|------------|
| **mistune** | Быстрый парсер, даёт токены с позициями (heading, paragraph, code_block) |
| **markdown-it-py** | CommonMark-совместимый |
| **mrkdwn_analysis** | Готовый анализатор секций |

```python
import mistune

class FragmentRenderer(mistune.BaseRenderer):
    def __init__(self):
        self.fragments = []
        self.current_line = 1

    def heading(self, text, level, raw=None):
        self.fragments.append({
            'kind': 'heading',
            'level': level,
            'content': raw,
            'line': self.current_line
        })
        return ''

    def paragraph(self, text):
        self.fragments.append({
            'kind': 'paragraph',
            'content': text,
            'line': self.current_line
        })
        return ''

    def block_code(self, code, info=None):
        self.fragments.append({
            'kind': 'code_block',
            'language': info,
            'content': code,
            'line': self.current_line
        })
        return ''
```

### 3. YAML/TOML/JSON (Config files)

| Библиотека | Особенности |
|------------|-------------|
| **ruamel.yaml** | Round-trip parsing, сохраняет комментарии и позиции |
| **tomllib** (stdlib 3.11+) | Для TOML |
| tree-sitter-yaml/json | Универсальный подход |

```python
from ruamel.yaml import YAML
from ruamel.yaml.comments import CommentedMap

yaml = YAML()
yaml.preserve_quotes = True

data = yaml.load(text)
# data.lc.line, data.lc.col — позиции для каждого ключа
```

### 4. HTML/XML

| Библиотека | Применение |
|------------|------------|
| **lxml.html** | Быстрый, XPath поддержка |
| **BeautifulSoup** | Толерантен к broken HTML |
| **html5lib** | HTML5-compliant |

```python
from lxml import html

tree = html.fromstring(content)
# Извлекаем семантические блоки
for elem in tree.iter('section', 'article', 'div', 'p', 'h1', 'h2', 'h3'):
    # elem.sourceline — номер строки
    fragments.append({
        'tag': elem.tag,
        'line': elem.sourceline,
        'content': html.tostring(elem, encoding='unicode')
    })
```

### 5. Plain Text (универсальный fallback)

| Библиотека | Назначение |
|------------|------------|
| **pySBD** | Sentence boundary detection (22 языка) |
| **NLTK PunktSentenceTokenizer** | ML-based sentence splitting |
| **spaCy Sentencizer** | Pipeline-совместимый |

```python
import pysbd

def split_plain_text(text: str, language: str = 'en') -> list[Fragment]:
    seg = pysbd.Segmenter(language=language, clean=False)
    sentences = seg.segment(text)

    fragments = []
    current_line = 1
    for sentence in sentences:
        # Группируем предложения в параграфы по \n\n
        fragments.append(Fragment(
            kind='sentence',
            content=sentence,
            start_line=current_line
        ))
        current_line += sentence.count('\n')

    return merge_into_paragraphs(fragments)
```

### 6. Проверка баланса скобок (для любого текста)

| Библиотека | Назначение |
|------------|------------|
| **strbalance** | Проверка баланса скобок, кавычек, HTML-тегов |

```python
from strbalance import Balance

bal = Balance()
result = bal.is_unbalanced(text)  # None если сбалансировано
```

## Архитектура классов

```python
from abc import ABC, abstractmethod
from pathlib import Path
from dataclasses import dataclass
from typing import Protocol

@dataclass
class Fragment:
    path: Path
    start_line: int
    end_line: int
    kind: str  # 'function', 'class', 'paragraph', 'section', 'chunk'
    content: str
    identifiers: frozenset[str] = frozenset()
    token_count: int = 0

class FragmentationStrategy(Protocol):
    """Протокол для стратегий фрагментации"""
    def can_handle(self, path: Path, content: str) -> bool: ...
    def fragment(self, path: Path, content: str) -> list[Fragment]: ...

class TreeSitterStrategy:
    """Стратегия для кода через tree-sitter"""

    SUPPORTED_EXTENSIONS = {
        '.py': 'python',
        '.js': 'javascript',
        '.ts': 'typescript',
        '.go': 'go',
        '.rs': 'rust',
        '.java': 'java',
        '.c': 'c',
        '.cpp': 'cpp',
        '.rb': 'ruby',
        # ... 50+ языков
    }

    def can_handle(self, path: Path, content: str) -> bool:
        return path.suffix.lower() in self.SUPPORTED_EXTENSIONS

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lang = self.SUPPORTED_EXTENSIONS[path.suffix.lower()]
        # Динамическая загрузка парсера
        parser = self._get_parser(lang)
        tree = parser.parse(content.encode('utf-8'))
        return self._extract_fragments(tree.root_node, content, path)

class MarkdownStrategy:
    """Стратегия для Markdown через mistune"""

    def can_handle(self, path: Path, content: str) -> bool:
        return path.suffix.lower() in {'.md', '.markdown', '.mdx'}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        # Используем mistune для парсинга
        fragments = []
        # ... парсинг через mistune
        return fragments

class ConfigStrategy:
    """Стратегия для конфигов (YAML, TOML, JSON)"""

    def can_handle(self, path: Path, content: str) -> bool:
        return path.suffix.lower() in {'.yaml', '.yml', '.toml', '.json'}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        if path.suffix.lower() in {'.yaml', '.yml'}:
            return self._fragment_yaml(path, content)
        # ...

class PlainTextStrategy:
    """Fallback стратегия для plain text"""

    def can_handle(self, path: Path, content: str) -> bool:
        return True  # Обрабатывает всё

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        # pySBD для sentence detection
        # Группировка в параграфы
        # Smart splitting по размеру
        pass

class FragmentationEngine:
    """Основной движок с chain of responsibility"""

    def __init__(self):
        # Порядок важен — от специфичных к общим
        self.strategies: list[FragmentationStrategy] = [
            TreeSitterStrategy(),
            MarkdownStrategy(),
            ConfigStrategy(),
            HTMLStrategy(),
            PlainTextStrategy(),  # Fallback
        ]

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        for strategy in self.strategies:
            if strategy.can_handle(path, content):
                try:
                    return strategy.fragment(path, content)
                except Exception as e:
                    logging.warning(f"Strategy {strategy} failed: {e}")
                    continue

        # Если всё упало — базовый chunk по строкам
        return self._fallback_chunk(path, content)
```

## План реализации

### Фаза 1: Подготовка (1-2 дня)

1. **Добавить зависимости в pyproject.toml:**
```toml
[project.optional-dependencies]
full = [
    "tree-sitter>=0.21",
    "tree-sitter-python>=0.21",
    "tree-sitter-javascript>=0.21",
    # ... другие языки
    "mistune>=3.0",
    "pysbd>=0.3",
    "ruamel.yaml>=0.18",
    "lxml>=5.0",
]
```

2. **Создать абстракции:**
   - `FragmentationStrategy` protocol
   - `Fragment` dataclass (расширить существующий)
   - `FragmentationEngine` orchestrator

### Фаза 2: Tree-sitter интеграция (2-3 дня)

1. Реализовать `TreeSitterStrategy`:
   - Динамическая загрузка парсеров по расширению
   - Извлечение function_definition, class_definition
   - Fallback на generic chunk для неподдерживаемых конструкций

2. Заменить:
   - `_compute_bracket_balance` → tree-sitter parse
   - `_find_balanced_end_line` → node.end_point
   - `_is_code_file` → TreeSitterStrategy.can_handle

### Фаза 3: Markup стратегии (1-2 дня)

1. `MarkdownStrategy` через mistune:
   - Парсинг headings, paragraphs, code blocks
   - Сохранение иерархии секций

2. `HTMLStrategy` через lxml:
   - Извлечение семантических блоков

3. `ConfigStrategy`:
   - YAML через ruamel.yaml (с позициями)
   - TOML/JSON через tree-sitter или stdlib

### Фаза 4: Plain Text fallback (1 день)

1. `PlainTextStrategy`:
   - pySBD для sentence detection
   - Параграфы по `\n\n`
   - Smart chunking по размеру с учётом предложений

### Фаза 5: Интеграция и тесты (2-3 дня)

1. Заменить текущую логику в `fragments.py` на `FragmentationEngine`
2. Добавить feature flags для optional зависимостей
3. Написать тесты для каждой стратегии
4. Benchmark производительности

## Миграционный путь

```python
# Текущий API остаётся совместимым
def fragment_file(path: Path, content: str) -> list[Fragment]:
    engine = FragmentationEngine()
    return engine.fragment(path, content)
```

## Обработка optional зависимостей

```python
class TreeSitterStrategy:
    def __init__(self):
        self._parsers = {}
        self._available = self._check_availability()

    def _check_availability(self) -> bool:
        try:
            import tree_sitter
            return True
        except ImportError:
            return False

    def can_handle(self, path: Path, content: str) -> bool:
        if not self._available:
            return False
        return path.suffix.lower() in self.SUPPORTED_EXTENSIONS
```

## Преимущества нового подхода

1. **Корректность**: tree-sitter понимает строки, комментарии, вложенность
2. **Расширяемость**: легко добавить новый формат через стратегию
3. **Производительность**: tree-sitter написан на C, очень быстрый
4. **Maintainability**: меньше кастомного кода, используем проверенные библиотеки
5. **Graceful degradation**: если библиотека недоступна, используем fallback

## Зависимости: размер и влияние

| Библиотека | Размер | Зависимости | Обязательная? |
|------------|--------|-------------|---------------|
| tree-sitter | ~2MB wheel | - | Нет (optional) |
| tree-sitter-python | ~200KB | tree-sitter | Нет |
| mistune | ~50KB | - | Нет |
| pysbd | ~100KB | - | Нет |
| ruamel.yaml | ~300KB | - | Нет |
| lxml | ~10MB wheel | libxml2 | Нет |

**Рекомендация**: Все библиотеки optional, базовый функционал работает без них через fallback.
