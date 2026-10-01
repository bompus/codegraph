//! Class-name calls whose method is declared on an ancestor.
use super::*;

impl KernelResolver {
    pub(super) fn inherited_class_method(&mut self, cls: &KNode, name: &str) -> Res<Option<Arc<KNode>>> {
        let key = (cls.id.clone(),name.to_string());
        if let Some(hit)=self.inherited_class_methods.get(&key) {return Ok(hit.clone());}
        let mut queue=VecDeque::from([(Arc::new(cls.clone()),0)]);
        let mut seen=HashSet::new();
        let mut result=None;
        while let Some((decl,depth))=queue.pop_front() {
            if depth>5 || !seen.insert(decl.id.clone()) {continue;}
            if depth>0 {
                let members=self.nodes_in_file(&decl.file_path)?;
                if let Some(m)=members.iter().find(|m| m.kind=="method" && m.name==name && m.qualified_name.rsplit_once("::").is_some_and(|(owner,_)| owner==decl.qualified_name)) {result=Some(m.clone());break;}
            }
            let mut parents=self.supertype_nodes(&decl.id)?;
            // Pascal's class heads survive even when extraction emitted no extends edge.
            if decl.language=="pascal" {
                if let Some(lines)=self.read_file(&decl.file_path) {
                    let from=(decl.start_line-1).max(0) as usize;
                    let to=((decl.start_line+2).max(0) as usize).min(lines.len());
                    let head=lines[from.min(to)..to].join(" ");
                    if let Some(m)=re!(r"(?i)=\s*class\s*\(([^)]*)\)").captures(&head) {
                        for parent in m[1].split(',').map(str::trim) {
                            let declarations: Vec<_>=self.nodes_by_name(parent)?.iter().filter(|n| n.language==decl.language && is_class_like(&n.kind)).cloned().collect();
                            if let [only]=declarations.as_slice() {parents.push(only.clone());}
                        }
                    }
                }
            }
            queue.extend(parents.into_iter().filter(|n| n.language==decl.language).map(|n|(n,depth+1)));
        }
        self.inherited_class_methods.insert(key,result.clone());Ok(result)
    }
}
